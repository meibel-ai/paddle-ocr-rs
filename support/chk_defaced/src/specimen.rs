//! Specimen-OCR escalation — the answer to the one case the deterministic outline cross-reference
//! cannot decide: a **fully custom font with no honest anchor**. When every code is remapped 1:1 and the
//! true letter never appears un-tampered anywhere in the document, no internal outline collision forms,
//! so [`crate::pdf_glyph`] / [`crate::docx_glyph`] stay silent. There is then no in-document ground
//! truth to compare against — so we manufacture one.
//!
//! For each distinct embedded glyph we render a **specimen** (the glyph itself, repeated on a line,
//! rasterized straight from its outline — no page, no pdfium) and OCR it. OCR recovers *what the glyph
//! actually draws*, independently of the font's lying cmap. If that disagrees with the character the
//! document claims to extract for that glyph, the font draws one letter and reports another: semantic
//! replacement, caught with **no honest anchor required**.
//!
//! Cost scales with the number of *distinct* glyphs, not the page count: identical outlines are OCR'd
//! once. Conservative by construction — an inconclusive/low-agreement OCR read never raises a finding.
//!
//! **Precision profile (be honest about it).** This is an *escalation*, not the default path. Recall is
//! high — it catches the no-anchor case the outline cross-reference structurally cannot (validated: 5/5
//! planted lies with zero honest mappings). Two filters keep precision in check:
//! - **ligatures excluded** — a ligature codepoint draws several letters, so single-letter OCR can never
//!   match it; comparing one is a guaranteed false positive.
//! - **confidence gate** ([`MIN_CONF`]) — genuine drawn letters OCR at ≥80 while the look-alike
//!   confusions (`c/e`, `s/f`, `ı/l`, `a/d`) land ≤47, so a gate in that gap drops them.
//!
//! Together these took a real LaTeX paper from 8 false positives to **0**, with the deterministic true
//! positives unchanged. The residual cost is *recall*: a glyph whose isolated shape OCRs below the gate
//! (e.g. a double-story `g`) is skipped rather than guessed. Net: trustworthy enough to surface, but
//! still an escalation for the cases the deterministic detectors can't decide — read findings as strong
//! candidates, not proof.

use std::collections::HashMap;

use anyhow::{Context, Result};

use crate::finding::{Category, Finding, Severity};
use crate::ocr::{GrayImage, OcrEngine, OcrHint};

const PX: f32 = 72.0; // glyph height in pixels (bigger reads more reliably)
const PAD: f32 = 12.0; // border around the specimen line
const GAP: f32 = 16.0; // spacing between repeated glyphs
const REPEATS: usize = 6; // how many copies of the glyph to put on the specimen line
// Discard OCR reads below this confidence (Tesseract 0–100). Chosen from the measured gap: genuine
// drawn letters score ≥80, while the cross-letter OCR confusions (c/e, s/f, ı/l, a/d) score ≤47.
const MIN_CONF: f32 = 65.0;

/// A typographic ligature codepoint (Alphabetic Presentation Forms: ﬀ ﬁ ﬂ ﬃ ﬄ ﬅ ﬆ …). Its glyph
/// legitimately draws *several* letters, so single-letter OCR can never agree with it — comparing one
/// would be a guaranteed false positive. These are normal typesetting, not semantic replacement, so we
/// never raise a finding for a claimed-ligature character.
fn is_ligature(c: char) -> bool {
    matches!(c as u32, 0xFB00..=0xFB4F)
}

/// Feeds a `ttf_parser` glyph outline into a `tiny_skia` path, mapping font units (Y-up) to image
/// pixels (Y-down) with the glyph's bounding box placed at `(PAD, PAD)`.
struct PathPen {
    pb: tiny_skia::PathBuilder,
    scale: f32,
    x_min: f32,
    y_max: f32,
}
impl PathPen {
    fn ix(&self, x: f32) -> f32 {
        (x - self.x_min) * self.scale + PAD
    }
    fn iy(&self, y: f32) -> f32 {
        (self.y_max - y) * self.scale + PAD
    }
}
impl ttf_parser::OutlineBuilder for PathPen {
    fn move_to(&mut self, x: f32, y: f32) {
        self.pb.move_to(self.ix(x), self.iy(y));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.pb.line_to(self.ix(x), self.iy(y));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.pb.quad_to(self.ix(x1), self.iy(y1), self.ix(x), self.iy(y));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.pb.cubic_to(self.ix(x1), self.iy(y1), self.ix(x2), self.iy(y2), self.ix(x), self.iy(y));
    }
    fn close(&mut self) {
        self.pb.close();
    }
}

/// Rasterize one glyph, repeated `REPEATS` times on a line, to a grayscale specimen image.
/// Returns `None` for empty/space glyphs (nothing to OCR).
fn render_specimen(face: &ttf_parser::Face, gid: u16) -> Option<GrayImage> {
    let bbox = face.glyph_bounding_box(ttf_parser::GlyphId(gid))?;
    let upem = face.units_per_em() as f32;
    if upem <= 0.0 {
        return None;
    }
    let scale = PX / upem;
    let gw = (bbox.x_max - bbox.x_min) as f32 * scale;
    let gh = (bbox.y_max - bbox.y_min) as f32 * scale;
    if gw < 1.0 || gh < 1.0 {
        return None;
    }

    let mut pen = PathPen {
        pb: tiny_skia::PathBuilder::new(),
        scale,
        x_min: bbox.x_min as f32,
        y_max: bbox.y_max as f32,
    };
    face.outline_glyph(ttf_parser::GlyphId(gid), &mut pen)?;
    let path = pen.pb.finish()?;

    let step = gw + GAP;
    let w = (PAD * 2.0 + gw + (REPEATS as f32 - 1.0) * step).ceil() as u32;
    let h = (gh + PAD * 2.0).ceil() as u32;
    let mut pm = tiny_skia::Pixmap::new(w.max(1), h.max(1))?;
    pm.fill(tiny_skia::Color::WHITE);

    let mut paint = tiny_skia::Paint::default();
    paint.set_color(tiny_skia::Color::BLACK);
    paint.anti_alias = true;
    for k in 0..REPEATS {
        let dx = k as f32 * step;
        pm.fill_path(
            &path,
            &paint,
            tiny_skia::FillRule::Winding,
            tiny_skia::Transform::from_translate(dx, 0.0),
            None,
        );
    }

    // RGBA8 (premultiplied, black-on-white) → 1-byte luma. The red channel suffices for grayscale ink.
    let gray: Vec<u8> = pm.data().chunks_exact(4).map(|px| px[0]).collect();
    Some(GrayImage::new(w, h, gray))
}

/// The single letter the OCR engine read from a specimen line, if it read one *confidently*. Tesseract
/// often groups the repeated glyphs into one or a few words, so we don't count instances against
/// `REPEATS`; instead we keep only reads at or above [`MIN_CONF`] (dropping the low-confidence `c/e`,
/// `s/f`, `ı/l`, `a/d` confusions) and require the dominant letter to be a **strict majority** of those
/// confident reads. No confident read, or a tie, returns `None` → no finding.
fn vote(scored: &[(char, f32)]) -> Option<char> {
    let mut counts: HashMap<char, usize> = HashMap::new();
    let mut total = 0usize;
    for &(c, conf) in scored {
        if conf < MIN_CONF || !c.is_alphabetic() {
            continue;
        }
        if let Some(l) = c.to_lowercase().next() {
            *counts.entry(l).or_default() += 1;
            total += 1;
        }
    }
    let (top, n) = counts.into_iter().max_by_key(|&(_, n)| n)?;
    (n * 2 > total).then_some(top) // strict majority among the confident reads
}

/// Run the specimen-OCR check over a set of embedded fonts, each given as its raw bytes plus the
/// `(claimed_char, glyph_id)` pairs the document extracts. `rule`/`label` tag the emitted findings.
///
/// For every distinct glyph: render → OCR → if the OCR letter disagrees (non-homoglyph) with a claimed
/// character, the font draws one letter and reports another.
pub fn specimen_scan(
    fonts: &[crate::FontClaims],
    ocr: &dyn OcrEngine,
    rule: &str,
    label: &str,
) -> Result<Vec<Finding>> {
    let mut lies: HashMap<(char, char), usize> = HashMap::new();

    for (bytes, claims) in fonts {
        let Ok(face) = ttf_parser::Face::parse(bytes, 0) else { continue };
        // distinct glyph → the claimed letters that reach it
        let mut by_gid: HashMap<u16, Vec<char>> = HashMap::new();
        for &(ch, gid) in claims {
            // Single base letters only: ligatures draw several letters (OCR can't match → false positive).
            if ch.is_alphabetic() && !is_ligature(ch) {
                by_gid.entry(gid).or_default().push(ch);
            }
        }
        for (gid, claimed) in by_gid {
            let Some(img) = render_specimen(&face, gid) else { continue };
            // Per-instance (letter, confidence); confidence-gated majority vote rejects shaky reads.
            let scored: Vec<(char, f32)> = ocr
                .recognize_scored(&img, OcrHint::SingleLine)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|(w, conf)| w.chars().find(|c| c.is_alphabetic()).map(|c| (c, conf)))
                .collect();
            let Some(drawn) = vote(&scored) else { continue }; // inconclusive/low-confidence → no finding
            for ch in claimed {
                if !crate::glyphmatch::legitimately_identical_ci(drawn, ch) {
                    // OCR sees `drawn`; the document extracts `ch` → the font claims a different letter.
                    let extracted = ch.to_lowercase().next().unwrap_or(ch);
                    *lies.entry((drawn, extracted)).or_default() += 1;
                }
            }
        }
    }

    let mut findings: Vec<Finding> = lies
        .into_iter()
        .map(|((drawn, extracted), n)| {
            Finding::new(
                rule,
                Severity::High,
                Category::FontIntegrity,
                label,
                format!(
                    "a glyph that OCRs as '{drawn}' is extracted as '{extracted}' ({n}×): the embedded font draws a different letter than it reports (semantic replacement confirmed by specimen OCR)"
                ),
                0.8,
            )
        })
        .collect();
    findings.sort_by(|a, b| a.message.cmp(&b.message));
    Ok(findings)
}

/// Convenience: scan a document's fonts by path, dispatching on extension. Builds the claim sets via the
/// same extraction the deterministic detectors use, then runs [`specimen_scan`].
pub fn specimen_scan_path(path: &std::path::Path, ocr: &dyn OcrEngine) -> Result<Vec<Finding>> {
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    let label = "embedded font";
    match ext.as_str() {
        "pdf" => {
            let fonts = crate::pdf_glyph::pdf_font_claims(path).context("extracting PDF font claims")?;
            specimen_scan(&fonts, ocr, "PDF.GLYPH_SEMANTIC_REPLACEMENT_OCR", label)
        }
        "docx" => {
            let fonts =
                crate::docx_glyph::docx_font_claims(path).context("extracting DOCX font claims")?;
            specimen_scan(&fonts, ocr, "DOCX.GLYPH_SEMANTIC_REPLACEMENT_OCR", label)
        }
        other => anyhow::bail!("specimen scan unsupported for .{other} (pdf, docx only)"),
    }
}

#[cfg(test)]
mod tests {
    use super::{is_ligature, vote, MIN_CONF};
    use crate::glyphmatch::legitimately_identical_ci as legitimately_identical;

    #[test]
    fn ligatures_are_excluded() {
        assert!(is_ligature('\u{FB00}')); // ﬀ
        assert!(is_ligature('\u{FB03}')); // ﬃ
        assert!(is_ligature('\u{FB04}')); // ﬄ
        assert!(!is_ligature('a'));
        assert!(!is_ligature('\u{00E9}')); // é — a normal accented letter, not a ligature
    }

    #[test]
    fn homoglyphs_and_real_swaps() {
        assert!(legitimately_identical('a', 'a'));
        assert!(legitimately_identical('M', 'm')); // case only
        assert!(legitimately_identical('i', 'l')); // common single-glyph pair
        assert!(legitimately_identical('a', '\u{0430}')); // Latin a vs Cyrillic а (different script)
        assert!(legitimately_identical('b', '\u{03B2}')); // Latin b vs Greek β (cross-script sharing)
        assert!(legitimately_identical('z', '\u{03B6}')); // Latin z vs Greek ζ
        assert!(legitimately_identical('\u{00F0}', '\u{0111}')); // ð vs đ — same ASCII base "d"
        assert!(legitimately_identical('\u{00F0}', '\u{0256}')); // ð vs ɖ — same ASCII base "d"
        assert!(!legitimately_identical('m', 'd')); // a genuine semantic swap (same script)
        assert!(!legitimately_identical('r', 'n'));
    }

    #[test]
    fn vote_needs_confident_strict_majority() {
        let hi = MIN_CONF + 10.0;
        let lo = MIN_CONF - 10.0;
        assert_eq!(vote(&[('m', hi)]), Some('m')); // one confident read is enough
        assert_eq!(vote(&[]), None); // nothing read
        assert_eq!(vote(&[('c', lo), ('c', lo)]), None); // all below the confidence floor
        assert_eq!(vote(&[('m', hi), ('m', hi), ('x', hi)]), Some('m')); // confident majority
        assert_eq!(vote(&[('m', hi), ('d', hi)]), None); // tie → inconclusive
        assert_eq!(vote(&[('m', hi), ('d', lo)]), Some('m')); // the low-confidence rival is discarded
    }
}
