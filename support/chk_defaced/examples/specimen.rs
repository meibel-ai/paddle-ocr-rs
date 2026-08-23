//! Specimen-OCR escalation: render the embedded font's own glyphs and OCR them to recover what each
//! glyph actually draws, then compare to what the document claims to extract. Catches custom fonts with
//! no honest anchor (the case the deterministic outline cross-reference cannot decide). No pdfium.
//!
//! Run: cargo run --example specimen --features ocr-specimen -- <pdf|docx>...
//! Env:
//!   TESSDATA_DIR  directory with the *.traineddata files (else crate default / TESSDATA_PREFIX)
//!   OCR_LANG      Tesseract language(s), `+`-joined (default: "ita+eng")

use std::path::PathBuf;

use chk_defaced::ocr::{OcrHint, TesseractOcr};
use chk_defaced::specimen;

fn main() -> anyhow::Result<()> {
    let tessdata = std::env::var("TESSDATA_DIR").ok().map(PathBuf::from);
    let lang = std::env::var("OCR_LANG").unwrap_or_else(|_| "ita+eng".to_string());
    // A specimen line is one glyph repeated → SingleLine (PSM 7) reads it best.
    let ocr = TesseractOcr::new(tessdata, &lang, OcrHint::SingleLine)?;

    for arg in std::env::args().skip(1) {
        let p = PathBuf::from(&arg);
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!("\n=== {name} ===");
        match specimen::specimen_scan_path(&p, &ocr) {
            Ok(findings) if findings.is_empty() => {
                println!("  glyphs draw what they claim (specimen OCR coherent)")
            }
            Ok(findings) => {
                for f in &findings {
                    println!("   [{:?}] {} — {}", f.severity, f.rule, f.message);
                }
            }
            Err(e) => println!("  error: {e:#}"),
        }
    }
    Ok(())
}
