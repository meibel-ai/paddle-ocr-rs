//! Unicode hygiene over extracted text (format-agnostic): flags characters that make "what is
//! extracted" diverge from "what is read" — PUA, zero-width/invisible, bidi overrides.

use crate::finding::{Category, Finding, Severity};
use crate::font::is_pua;

/// Common zero-width and invisible characters.
fn is_zero_width(cp: u32) -> bool {
    matches!(cp,
        0x200B..=0x200D   // ZWSP/ZWNJ/ZWJ
        | 0x2060..=0x2064 // word joiner, invisible operators
        | 0xFEFF          // BOM / zero-width no-break space
        | 0x00AD          // soft hyphen
    )
}

/// Directional overrides/embeddings (can reverse the visual order of text).
fn is_bidi_override(cp: u32) -> bool {
    matches!(cp, 0x202A..=0x202E | 0x2066..=0x2069)
}

/// Analyze an extracted string and produce findings (with a caller-provided `location`).
pub fn scan_text(text: &str, location: &str, out: &mut Vec<Finding>) {
    let mut pua = 0usize;
    let mut zw = 0usize;
    let mut bidi = 0usize;
    for ch in text.chars() {
        let cp = ch as u32;
        if is_pua(cp) {
            pua += 1;
        } else if is_zero_width(cp) {
            zw += 1;
        } else if is_bidi_override(cp) {
            bidi += 1;
        }
    }
    // Threshold: a handful of PUA characters can be legitimate (private symbols); a deliberate
    // obfuscation produces many. Require >= 4 to avoid false positives on clean documents.
    if pua >= 4 {
        out.push(Finding::new(
            "UNICODE.PUA",
            Severity::High,
            Category::UnicodeHygiene,
            location,
            format!("{pua} Private Use Area character(s) in the extracted text ('private', unreadable codepoints without the font cmap → typical of obfuscation)"),
            0.9,
        ));
    }
    if zw > 0 {
        out.push(Finding::new(
            "UNICODE.ZERO_WIDTH",
            Severity::Medium,
            Category::UnicodeHygiene,
            location,
            format!("{zw} invisible (zero-width) character(s) in the extracted text"),
            0.7,
        ));
    }
    if bidi > 0 {
        out.push(Finding::new(
            "UNICODE.BIDI_OVERRIDE",
            Severity::Medium,
            Category::UnicodeHygiene,
            location,
            format!("{bidi} bidi directional override(s) (can reverse the visual order of text)"),
            0.7,
        ));
    }
}
