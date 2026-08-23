//! PDF embedded-font semantic-replacement detector (variant A3) — deterministic, NO OCR and NO
//! rendering, font-agnostic. The PDF counterpart of [`crate::docx_glyph`].
//!
//! For every embedded font it combines three PDF/​SFNT layers:
//! - `ToUnicode`  : code → the **extracted** character (what a RAG/LLM reads);
//! - code → glyph : the embedded font's internal cmap and, for CID fonts, the Identity CID→GID map;
//! - `glyf`/CFF   : glyph → **outline** (what is drawn).
//!
//! It hashes each glyph's outline and cross-references it across all the document's fonts: when the
//! same outline is reachable from two different (non-homoglyph) letters, the extracted letter disagrees
//! with the drawn one — semantic replacement. Deterministic; OCR stays as a fallback for fonts whose
//! outlines cannot be read (Type3, bare CFF without a usable program, etc.).

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use anyhow::{Context, Result};
use lopdf::{Dictionary, Document, Object};
use regex::Regex;

use crate::finding::{Category, Finding, OutlineScan, Severity};

// ToUnicode CMap regexes — compiled once (were re-compiled per embedded font).
static BFCHAR_BLK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)beginbfchar(.*?)endbfchar").unwrap());
static BFRANGE_BLK: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?s)beginbfrange(.*?)endbfrange").unwrap());
static PAIR: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>").unwrap());
static TRIPLE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>\s*<([0-9A-Fa-f]+)>").unwrap());

// ── lopdf helpers ────────────────────────────────────────────────────────────
fn deref<'a>(doc: &'a Document, o: &'a Object) -> Option<&'a Object> {
    match o {
        Object::Reference(id) => doc.get_object(*id).ok(),
        x => Some(x),
    }
}
fn dget<'a>(doc: &'a Document, d: &'a Dictionary, k: &[u8]) -> Option<&'a Object> {
    d.get(k).ok().and_then(|o| deref(doc, o))
}
fn as_dict(o: &Object) -> Option<&Dictionary> {
    match o {
        Object::Dictionary(d) => Some(d),
        Object::Stream(s) => Some(&s.dict),
        _ => None,
    }
}
fn name_is(o: &Object, want: &str) -> bool {
    matches!(o, Object::Name(b) if b == want.as_bytes())
}

/// Descendant CIDFont dict (Type0) or the dict itself (simple font).
fn descendant<'a>(doc: &'a Document, fontdict: &'a Dictionary) -> Option<&'a Dictionary> {
    if let Some(Object::Array(arr)) = dget(doc, fontdict, b"DescendantFonts") {
        return arr.first().and_then(|o| deref(doc, o)).and_then(as_dict);
    }
    Some(fontdict)
}

fn font_descriptor<'a>(doc: &'a Document, fontdict: &'a Dictionary) -> Option<&'a Dictionary> {
    let d = descendant(doc, fontdict).unwrap_or(fontdict);
    dget(doc, d, b"FontDescriptor").and_then(as_dict)
}

fn embedded_font_bytes(doc: &Document, fontdict: &Dictionary) -> Option<Vec<u8>> {
    let fd = font_descriptor(doc, fontdict)?;
    for key in [b"FontFile2".as_ref(), b"FontFile3".as_ref(), b"FontFile".as_ref()] {
        if let Some(Object::Stream(s)) = dget(doc, fd, key) {
            if let Ok(b) = s.decompressed_content() {
                return Some(b);
            }
        }
    }
    None
}

/// CID→GID resolution for a Type0 font. `Identity` means GID == code; `Map` is an explicit
/// `CIDToGIDMap` stream (big-endian u16 GID at byte offset `2*CID`). With Identity-H encoding the PDF
/// character code equals the CID, so both `ToUnicode` codes and this map are keyed by the same code.
enum CidGid {
    Identity,
    Map(Vec<u16>),
}

impl CidGid {
    /// GID for a character code, or `None` for `.notdef`/out-of-range (so callers skip it).
    fn gid(&self, code: u32) -> Option<u16> {
        match self {
            CidGid::Identity => u16::try_from(code).ok(),
            CidGid::Map(m) => m.get(code as usize).copied().filter(|&g| g != 0),
        }
    }
}

/// `true` for a Type0 (composite/CID) font — it has a `DescendantFonts` array. Only for these does a
/// content-stream code map to a glyph id via Identity-H + `CIDToGIDMap`; a *simple* font routes code →
/// glyph through `/Encoding`, so its `ToUnicode` codes are **not** glyph ids and must not be hashed as
/// such (doing so manufactured spurious cross-reference collisions on real subset fonts).
pub(crate) fn is_type0(doc: &Document, fontdict: &Dictionary) -> bool {
    matches!(dget(doc, fontdict, b"DescendantFonts"), Some(Object::Array(_)))
}

/// Read a font's `CIDToGIDMap`: the `/Identity` name (or absent) → [`CidGid::Identity`]; a stream →
/// [`CidGid::Map`]. Closing the earlier gap where a non-identity stream map was skipped entirely
/// (so subset CID fonts with no internal cmap escaped detection).
fn cid_to_gid(doc: &Document, fontdict: &Dictionary) -> CidGid {
    let Some(d) = descendant(doc, fontdict) else { return CidGid::Identity };
    match dget(doc, d, b"CIDToGIDMap") {
        Some(Object::Stream(s)) => match s.decompressed_content() {
            Ok(bytes) => {
                CidGid::Map(bytes.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect())
            }
            Err(_) => CidGid::Identity,
        },
        _ => CidGid::Identity, // /Identity name, absent, or unexpected → Identity default
    }
}

/// Parse `ToUnicode` into code → character (bfchar `<code> <utf16be>` and bfrange `<lo> <hi> <start>`).
pub(crate) fn parse_tounicode(doc: &Document, fontdict: &Dictionary) -> HashMap<u32, char> {
    let mut map = HashMap::new();
    let Some(Object::Stream(s)) = dget(doc, fontdict, b"ToUnicode") else { return map };
    let Ok(content) = s.decompressed_content() else { return map };
    let text = String::from_utf8_lossy(&content);

    let hex_char = |h: &str| -> Option<char> {
        let v = u32::from_str_radix(h.get(0..4)?, 16).ok()?;
        char::from_u32(v)
    };
    // bfchar
    for blk in BFCHAR_BLK.captures_iter(&text) {
        for c in PAIR.captures_iter(&blk[1]) {
            if let (Ok(code), Some(ch)) = (u32::from_str_radix(&c[1], 16), hex_char(&c[2])) {
                map.insert(code, ch);
            }
        }
    }
    // bfrange <lo> <hi> <start>
    for blk in BFRANGE_BLK.captures_iter(&text) {
        for c in TRIPLE.captures_iter(&blk[1]) {
            if let (Ok(lo), Ok(hi), Some(startc)) =
                (u32::from_str_radix(&c[1], 16), u32::from_str_radix(&c[2], 16), hex_char(&c[3]))
            {
                let start = startc as u32;
                for (k, code) in (lo..=hi.min(lo + 4096)).enumerate() {
                    if let Some(ch) = char::from_u32(start + k as u32) {
                        map.insert(code, ch);
                    }
                }
            }
        }
    }
    map
}

use crate::font::glyph_outline_hash;
use crate::glyphmatch::{legitimately_identical, letter_latin as letter};

/// Per embedded font, the raw font bytes plus the `(claimed_char, glyph_id)` pairs the document
/// extracts — the input the specimen-OCR escalation ([`crate::specimen`]) needs to recover, by OCR,
/// what each glyph actually draws and compare it to what is claimed. Same extraction as
/// [`pdf_outline_scan`] (ToUnicode + Identity CID→GID, and the font's internal Unicode cmap), but it
/// records the claims instead of cross-referencing outlines, so it catches fonts with no honest anchor.
pub fn pdf_font_claims(path: &Path) -> Result<Vec<crate::FontClaims>> {
    let doc = Document::load(path).with_context(|| format!("opening PDF {}", path.display()))?;
    Ok(pdf_font_claims_doc(&doc))
}

/// [`pdf_font_claims`] on an already-loaded document (avoids re-parsing the PDF).
pub fn pdf_font_claims_doc(doc: &Document) -> Vec<crate::FontClaims> {
    use std::collections::HashSet;
    let mut out: Vec<crate::FontClaims> = Vec::new();

    for obj in doc.objects.values() {
        let Some(d) = as_dict(obj) else { continue };
        if dget(doc, d, b"Type").map(|o| name_is(o, "Font")) != Some(true) {
            continue;
        }
        let Some(bytes) = embedded_font_bytes(doc, d) else { continue };
        if ttf_parser::Face::parse(&bytes, 0).is_err() {
            continue;
        }
        let mut claims: HashSet<(char, u16)> = HashSet::new();

        // ToUnicode (code → extracted char) + CID→GID (code → glyph), Type0 only (see `is_type0`):
        // for simple fonts the code is not a glyph id.
        if is_type0(doc, d) {
            let cidgid = cid_to_gid(doc, d);
            for (code, ch) in parse_tounicode(doc, d) {
                if let Some(gid) = cidgid.gid(code) {
                    claims.insert((ch, gid));
                }
            }
        }
        if let Ok(face) = ttf_parser::Face::parse(&bytes, 0) {
            if let Some(cmap) = face.tables().cmap {
                for sub in cmap.subtables {
                    if !sub.is_unicode() {
                        continue;
                    }
                    sub.codepoints(|cp| {
                        if let (Some(c), Some(gid)) = (char::from_u32(cp), sub.glyph_index(cp)) {
                            claims.insert((c, gid.0));
                        }
                    });
                }
            }
        }
        if !claims.is_empty() {
            out.push((bytes, claims.into_iter().collect()));
        }
    }
    out
}

/// **Canonical tamper check** for a PDF (deterministic, no OCR): for every embedded font whose family
/// is in `registry`, compare its glyph outlines to the canonical ones and return the
/// `(extracted_char, true_drawn_char)` substitutions where a glyph draws a different letter. Empty when
/// no embedded font is registry-known (→ doubt; resolve with OCR).
pub fn pdf_canonical_substitutions(path: &Path, registry: &crate::registry::FontRegistry) -> Result<Vec<(char, char)>> {
    let doc = Document::load(path).with_context(|| format!("opening PDF {}", path.display()))?;
    Ok(pdf_canonical_substitutions_doc(&doc, registry))
}

/// [`pdf_canonical_substitutions`] on an already-loaded document (avoids re-parsing the PDF).
pub fn pdf_canonical_substitutions_doc(doc: &Document, registry: &crate::registry::FontRegistry) -> Vec<(char, char)> {
    let mut out = Vec::new();
    for obj in doc.objects.values() {
        let Some(d) = as_dict(obj) else { continue };
        if dget(doc, d, b"Type").map(|o| name_is(o, "Font")) != Some(true) {
            continue;
        }
        let Some(bytes) = embedded_font_bytes(doc, d) else { continue };
        for pf in crate::font::parse_data(&bytes) {
            out.extend(registry.canonical_substitutions(&pf));
        }
    }
    out.sort_unstable();
    out.dedup();
    out
}

/// Scan a PDF's embedded fonts for semantic replacement (variant A3). Deterministic, no OCR.
pub fn pdf_outline_scan(path: &Path) -> Result<OutlineScan> {
    let doc = Document::load(path).with_context(|| format!("opening PDF {}", path.display()))?;
    Ok(pdf_outline_scan_doc(&doc))
}

/// [`pdf_outline_scan`] on an already-loaded document (avoids re-parsing the PDF).
/// Detect a uniform alphabetic shift in a recovered substitution map (`(extracted, drawn)` pairs). Returns
/// `Some(shift)` when a single nonzero shift `drawn - extracted` accounts for at least `MIN_SHIFT_LETTERS`
/// ascii-letter pairs AND at least `SHIFT_DOMINANCE` of all ascii-letter pairs — the signature of a
/// font-encoding artifact rather than a targeted replacement. See the call site for rationale.
fn uniform_shift(subs: &[(char, char)]) -> Option<i32> {
    const MIN_SHIFT_LETTERS: usize = 8;
    const SHIFT_DOMINANCE: f64 = 0.8;
    let mut counts: HashMap<i32, usize> = HashMap::new();
    let mut total = 0usize;
    for &(lie, truth) in subs {
        if lie.is_ascii_alphabetic() && truth.is_ascii_alphabetic() {
            let shift = truth.to_ascii_lowercase() as i32 - lie.to_ascii_lowercase() as i32;
            *counts.entry(shift).or_default() += 1;
            total += 1;
        }
    }
    let (&shift, &n) = counts.iter().max_by_key(|(_, n)| **n)?;
    if shift != 0 && n >= MIN_SHIFT_LETTERS && n as f64 >= SHIFT_DOMINANCE * total as f64 {
        Some(shift)
    } else {
        None
    }
}

pub fn pdf_outline_scan_doc(doc: &Document) -> OutlineScan {
    let mut sigs: HashMap<u64, HashMap<char, usize>> = HashMap::new();

    for obj in doc.objects.values() {
        let Some(d) = as_dict(obj) else { continue };
        if dget(doc, d, b"Type").map(|o| name_is(o, "Font")) != Some(true) {
            continue;
        }
        let Some(bytes) = embedded_font_bytes(doc, d) else { continue };
        let Ok(face) = ttf_parser::Face::parse(&bytes, 0) else { continue };

        // (1) ToUnicode + CID→GID: code → letter, code → GID → outline. Type0 only (see `is_type0`):
        // a simple font's content-stream code is not a glyph id (it routes through /Encoding), so
        // hashing the glyph at `code` would compare the wrong outline and fabricate collisions.
        if is_type0(doc, d) {
            let cidgid = cid_to_gid(doc, d);
            for (code, ch) in parse_tounicode(doc, d) {
                let Some(letter) = letter(ch as u32) else { continue };
                if let Some(gid) = cidgid.gid(code) {
                    if let Some(h) = glyph_outline_hash(&face, gid) {
                        *sigs.entry(h).or_default().entry(letter).or_default() += 1;
                    }
                }
            }
        }

        // (2) the font's own internal Unicode cmap (simple TrueType fonts): codepoint → GID → outline.
        if let Some(cmap) = face.tables().cmap {
            for sub in cmap.subtables {
                if !sub.is_unicode() {
                    continue;
                }
                sub.codepoints(|cp| {
                    let Some(letter) = letter(cp) else { return };
                    if let Some(gid) = sub.glyph_index(cp) {
                        if let Some(h) = glyph_outline_hash(&face, gid.0) {
                            *sigs.entry(h).or_default().entry(letter).or_default() += 1;
                        }
                    }
                });
            }
        }
    }

    // Collisions: one outline reachable from >1 non-homoglyph letter → semantic replacement.
    let mut lies: HashMap<(char, char), usize> = HashMap::new();
    for tally in sigs.values() {
        if tally.len() < 2 {
            continue;
        }
        let mut letters: Vec<(char, usize)> = tally.iter().map(|(c, n)| (*c, *n)).collect();
        letters.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        let (truth, _) = letters[0];
        for (lie, n) in letters.iter().skip(1) {
            if legitimately_identical(truth, *lie) {
                continue;
            }
            *lies.entry((truth, *lie)).or_default() += *n;
        }
    }

    // Substitution map: for each extracted `lie`, the most frequent drawn `truth` is the correction.
    let mut best: HashMap<char, (char, usize)> = HashMap::new();
    for (&(truth, lie), &n) in &lies {
        let e = best.entry(lie).or_insert((truth, 0));
        if n > e.1 {
            *e = (truth, n);
        }
    }
    let mut substitutions: Vec<(char, char)> = best.into_iter().map(|(lie, (truth, _))| (lie, truth)).collect();
    substitutions.sort_unstable();

    // A recovered map that is a single UNIFORM alphabetic shift over many letters (every extracted letter
    // draws the one N positions away) is a FONT-ENCODING / SUBSETTING ARTIFACT (typical of LaTeX papers),
    // NOT a targeted A3 attack: a real replacement is surgical — a few letters that turn one plausible
    // word into another — never a global Caesar shift, which would garble every visible word. Suppress the
    // High collision findings and the substitution map, emit one benign Info note instead. Deterministic
    // and O(n): no OCR, no rendering.
    if let Some(shift) = uniform_shift(&substitutions) {
        let n = substitutions.len();
        return OutlineScan {
            findings: vec![Finding::new(
                "PDF.FONT_ENCODING_ARTIFACT",
                Severity::Info,
                Category::Structural,
                "embedded fonts",
                format!(
                    "uniform alphabetic shift ({shift:+}) across {n} letters — a font-encoding/subsetting artifact (e.g. LaTeX), not a targeted replacement: the extracted text matches what is rendered"
                ),
                0.85,
            )],
            substitutions: Vec::new(),
        };
    }

    let mut findings: Vec<Finding> = lies
        .into_iter()
        .map(|((truth, lie), n)| {
            Finding::new(
                "PDF.GLYPH_SEMANTIC_REPLACEMENT",
                Severity::High,
                Category::FontIntegrity,
                "embedded font",
                format!(
                    "a glyph that draws '{truth}' is mapped from the extracted character '{lie}' ({n}×): the embedded font lies (semantic replacement / variant A3)"
                ),
                0.9,
            )
        })
        .collect();
    findings.sort_by(|a, b| a.message.cmp(&b.message));
    OutlineScan { findings, substitutions }
}

#[cfg(test)]
mod tests {
    use super::{uniform_shift, CidGid};

    #[test]
    fn cidgid_identity_is_code() {
        let m = CidGid::Identity;
        assert_eq!(m.gid(65), Some(65));
        assert_eq!(m.gid(0), Some(0));
        assert_eq!(m.gid(70_000), None); // beyond u16
    }

    #[test]
    fn uniform_shift_is_detected_as_artifact() {
        // The ROPOLL case: every extracted letter draws the one 2 positions earlier (c→a, d→b, …).
        let subs: Vec<(char, char)> = "cdfgijklmnopqrstuwxz"
            .chars()
            .map(|c| (c, ((c as u8) - 2) as char))
            .collect();
        assert_eq!(uniform_shift(&subs), Some(-2));
    }

    #[test]
    fn surgical_replacement_is_not_a_uniform_shift() {
        // A real, targeted swap (a handful of unrelated pairs) is not a uniform shift → stays flagged.
        let subs = vec![('a', 'o'), ('e', 'a'), ('r', 'n')];
        assert_eq!(uniform_shift(&subs), None);
    }

    #[test]
    fn identity_shift_is_not_flagged() {
        // Same letter mapped to itself is shift 0 (not an artifact signal).
        let subs: Vec<(char, char)> = ('a'..='m').map(|c| (c, c)).collect();
        assert_eq!(uniform_shift(&subs), None);
    }

    #[test]
    fn cidgid_stream_map_resolves_and_filters() {
        // CIDToGIDMap: CID 0 → .notdef(0), CID 1 → GID 5, CID 2 → GID 300.
        let m = CidGid::Map(vec![0, 5, 300]);
        assert_eq!(m.gid(0), None); // .notdef filtered out
        assert_eq!(m.gid(1), Some(5));
        assert_eq!(m.gid(2), Some(300));
        assert_eq!(m.gid(9), None); // out of range
    }
}
