//! Glyph-atlas detector (feature `ocr-atlas`): builds a dictionary of the DRAWN glyphs and compares
//! it to the EXTRACTED (claimed) characters, applying OCR **only to the ambiguous glyphs**.
//!
//! 1. Render the pages with pdfium and cluster every text character by the *image of its glyph*
//!    (perceptual hash of the rendered crop). One visual glyph must always extract to the same letter.
//! 2. A glyph cluster extracted as more than one letter is **ambiguous** — even 2 instances differing
//!    from 500 is suspect: the majority is NOT assumed to be the truth.
//! 3. For each ambiguous glyph, OCR ONE instance to resolve the true letter, then flag every extraction
//!    that disagrees with it (the lying — semantic-replacement — codes, variant A3).
//!
//! OCR runs once per ambiguous glyph (deduplicated), so its cost scales with the tampering, not with
//! the document size.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{anyhow, Result};
use image::imageops::{crop_imm, resize, FilterType};
use image::GrayImage as ImgGray;
use pdfium_render::prelude::*;

use crate::finding::{Category, Finding, Severity};
use crate::ocr::{GrayImage, OcrEngine, OcrHint};

/// Measurements for the glyph scan.
#[derive(Debug, Clone, Copy)]
pub struct GlyphStats {
    pub total_chars: usize,
    pub unique_glyphs: usize,
    pub ambiguous_glyphs: usize,
    pub ocr_calls: usize,
}

struct Cluster {
    total: usize,
    claims: HashMap<char, usize>,
    sample: ImgGray,
}

/// 256-bit average hash (16×16) of a glyph crop → exact-equality cluster key (negligible collision).
fn ahash(img: &ImgGray) -> [u64; 4] {
    let small = resize(img, 16, 16, FilterType::Triangle);
    let px = small.as_raw();
    let mean: u32 = px.iter().map(|&p| p as u32).sum::<u32>() / (px.len() as u32).max(1);
    let mut bits = [0u64; 4];
    for (i, &p) in px.iter().enumerate() {
        if (p as u32) < mean {
            bits[i / 64] |= 1 << (i % 64);
        }
    }
    bits
}

/// Prepare a single-glyph crop for OCR: upscale and pad with a white border (Tesseract needs margin).
fn prep_for_ocr(crop: &ImgGray) -> GrayImage {
    let target_h = 64u32;
    let scale = target_h as f32 / crop.height().max(1) as f32;
    let w = ((crop.width() as f32 * scale).round() as u32).max(1);
    let up = resize(crop, w, target_h, FilterType::Lanczos3);
    let pad = 20u32;
    let (pw, ph) = (w + 2 * pad, target_h + 2 * pad);
    let mut buf = vec![255u8; (pw * ph) as usize];
    for y in 0..target_h {
        for x in 0..w {
            buf[((y + pad) * pw + (x + pad)) as usize] = up.get_pixel(x, y)[0];
        }
    }
    GrayImage::new(pw, ph, buf)
}

fn first_letter(s: &str) -> Option<char> {
    s.chars().find(|c| c.is_alphabetic()).map(|c| c.to_ascii_lowercase())
}

/// Run the glyph-atlas scan. `ocr` is used only to resolve ambiguous glyphs.
pub fn glyph_scan(
    pdf: &Path,
    pdfium_dir: &Path,
    ocr: &dyn OcrEngine,
    scale: f32,
    max_pages: usize,
) -> Result<(Vec<Finding>, GlyphStats)> {
    let lib = Pdfium::pdfium_platform_library_name_at_path(pdfium_dir);
    let bindings =
        Pdfium::bind_to_library(&lib).map_err(|e| anyhow!("bind pdfium ({}): {e}", lib.display()))?;
    let pdfium = Pdfium::new(bindings);
    let doc = pdfium.load_pdf_from_file(pdf, None).map_err(|e| anyhow!("open pdf: {e}"))?;

    let mut clusters: HashMap<[u64; 4], Cluster> = HashMap::new();
    let mut total_chars = 0usize;

    for (pi, page) in doc.pages().iter().enumerate() {
        if pi >= max_pages {
            break;
        }
        let h_pt = page.height().value;
        let target_w = ((page.width().value * scale).round() as i32).max(1);
        let cfg = PdfRenderConfig::new().set_target_width(target_w);
        let bitmap = page.render_with_config(&cfg).map_err(|e| anyhow!("render: {e}"))?;
        let luma = bitmap.as_image().to_luma8();
        let (iw, ih) = (luma.width(), luma.height());
        let sx = iw as f32 / page.width().value.max(1.0);
        let sy = ih as f32 / h_pt.max(1.0);

        let Ok(text) = page.text() else { continue };
        for ch in text.chars().iter() {
            let Some(c) = ch.unicode_char() else { continue };
            if c.is_whitespace() || c.is_control() {
                continue;
            }
            let Ok(b) = ch.tight_bounds() else { continue };
            let x0 = (b.left().value * sx).floor().max(0.0) as u32;
            let x1 = (b.right().value * sx).ceil().min(iw as f32) as u32;
            let y0 = ((h_pt - b.top().value) * sy).floor().max(0.0) as u32;
            let y1 = ((h_pt - b.bottom().value) * sy).ceil().min(ih as f32) as u32;
            if x1 <= x0 + 1 || y1 <= y0 + 1 {
                continue;
            }
            let crop = crop_imm(&luma, x0, y0, x1 - x0, y1 - y0).to_image();
            let key = ahash(&crop);
            let e = clusters.entry(key).or_insert_with(|| Cluster {
                total: 0,
                claims: HashMap::new(),
                sample: crop.clone(),
            });
            e.total += 1;
            *e.claims.entry(c).or_default() += 1;
            total_chars += 1;
        }
    }

    let unique_glyphs = clusters.len();
    let mut findings = Vec::new();
    let mut ocr_calls = 0usize;
    let mut ambiguous = 0usize;

    for cluster in clusters.values() {
        let distinct: Vec<(char, usize)> = cluster
            .claims
            .iter()
            .filter(|(k, _)| k.is_alphabetic())
            .map(|(k, v)| (*k, *v))
            .collect();
        if distinct.len() < 2 {
            continue; // consistent glyph → trusted, no OCR
        }
        ambiguous += 1;

        // Resolve the TRUE letter via OCR (never trust the majority blindly).
        ocr_calls += 1;
        let ocr_text = ocr.recognize(&prep_for_ocr(&cluster.sample), OcrHint::SingleChar)?;
        let truth = first_letter(&ocr_text)
            .filter(|t| cluster.claims.keys().any(|k| k.to_ascii_lowercase() == *t))
            .or_else(|| {
                // OCR inconclusive → fall back to the most frequent claim.
                distinct.iter().max_by_key(|(_, n)| *n).map(|(c, _)| c.to_ascii_lowercase())
            });
        let Some(truth) = truth else { continue };

        for (claimed, n) in &distinct {
            if claimed.to_ascii_lowercase() != truth {
                findings.push(Finding::new(
                    "GLYPH.SEMANTIC_REPLACEMENT",
                    Severity::High,
                    Category::FontIntegrity,
                    "glifo",
                    format!(
                        "a glyph that renders '{truth}' is extracted as '{claimed}' ({n}×): the text layer diverges from what is drawn (semantic replacement / variant A3)"
                    ),
                    0.9,
                ));
            }
        }
    }

    Ok((
        findings,
        GlyphStats { total_chars, unique_glyphs, ambiguous_glyphs: ambiguous, ocr_calls },
    ))
}
