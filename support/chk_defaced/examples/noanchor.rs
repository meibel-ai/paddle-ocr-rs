//! No-anchor validation (the case the deterministic outline cross-reference structurally cannot catch).
//!
//! Build claims that lie — claim extracted letter X but point at the glyph of a DIFFERENT letter Y —
//! with NO honest Y→Y mapping anywhere. The deterministic detector needs an outline reachable from two
//! letters to fire; here every glyph is reached from exactly one, so it would find nothing. specimen-OCR
//! reads the glyph directly and should still catch every planted lie.
//!
//! Run: cargo run --example noanchor --features ocr-specimen -- [font.ttf]

use std::path::PathBuf;

use chk_defaced::ocr::{OcrHint, TesseractOcr};
use chk_defaced::specimen;

fn main() -> anyhow::Result<()> {
    let font = std::env::args().nth(1).unwrap_or_else(|| r"C:\Windows\Fonts\arial.ttf".to_string());
    let bytes = std::fs::read(&font)?;
    let face = ttf_parser::Face::parse(&bytes, 0)?;

    // (extracted/claimed letter, actually-drawn letter). Drawn letters chosen to be OCR-robust — the
    // point is the no-anchor topology, not OCR's weakness on pathological isolated shapes (e.g. a
    // double-story 'g' scores too low in isolation and is intentionally filtered by the confidence gate).
    let pairs = [('a', 'm'), ('c', 'w'), ('l', 'r'), ('x', 'k'), ('q', 'b')];
    let mut claims = Vec::new();
    for (claimed, drawn) in pairs {
        if let Some(g) = face.glyph_index(drawn) {
            claims.push((claimed, g.0)); // claim `claimed`, but the gid draws `drawn`
        }
    }
    let fonts = vec![(bytes, claims)];

    let tessdata = std::env::var("TESSDATA_DIR").ok().map(PathBuf::from);
    let lang = std::env::var("OCR_LANG").unwrap_or_else(|_| "ita+eng".to_string());
    let ocr = TesseractOcr::new(tessdata, &lang, OcrHint::SingleLine)?;

    let findings = specimen::specimen_scan(&fonts, &ocr, "TEST.NOANCHOR", "synthetic font")?;
    println!("planted lies (extracted <- drawn): {pairs:?}");
    println!("no honest mapping provided → the deterministic cross-reference cannot fire by construction.");
    if findings.is_empty() {
        println!("RESULT: specimen-OCR found NOTHING (a miss).");
    } else {
        println!("RESULT: specimen-OCR caught {} of {}:", findings.len(), pairs.len());
        for f in &findings {
            println!("  {}", f.message);
        }
    }
    Ok(())
}
