//! OCR-atlas experiment on PDFs: render with pdfium, OCR the rendering, compare with the extracted
//! text layer. Flags pages where what is *seen* diverges from what is *extracted* (variant A3/C).
//!
//! Run:
//!   cargo run --example atlas --features ocr-atlas -- <pdf>...
//! Env:
//!   PDFIUM_DIR   directory containing the pdfium dynamic library (default: pageindex native/arm64)
//!   TESSDATA_DIR directory containing the *.traineddata files (else the crate default / TESSDATA_PREFIX)
//!   OCR_LANG     Tesseract language(s), `+`-joined (default: "ita+eng")

use std::path::{Path, PathBuf};

use chk_defaced::atlas;
use chk_defaced::ocr::{OcrHint, TesseractOcr};

fn snippet(s: &str) -> String {
    s.split_whitespace().take(16).collect::<Vec<_>>().join(" ")
}

fn main() -> anyhow::Result<()> {
    let pdfium_dir = std::env::var("PDFIUM_DIR")
        .unwrap_or_else(|_| r"C:\Progetti\pageindex-rs\native\arm64".to_string());
    let tessdata = std::env::var("TESSDATA_DIR").ok().map(PathBuf::from);
    let max_pages: usize = std::env::var("ATLAS_MAX_PAGES").ok().and_then(|v| v.parse().ok()).unwrap_or(3);
    // OCR languages (Tesseract `+`-joined, e.g. "ita+eng"); override with OCR_LANG.
    let lang = std::env::var("OCR_LANG").unwrap_or_else(|_| "ita+eng".to_string());
    let ocr = TesseractOcr::new(tessdata, &lang, OcrHint::Block)?;

    for arg in std::env::args().skip(1) {
        let pdf = PathBuf::from(&arg);
        println!("\n=== {} ===", pdf.display());
        match atlas::ocr_vs_extracted(&pdf, Path::new(&pdfium_dir), &ocr, 2.0, max_pages) {
            Ok(pages) => {
                for p in &pages {
                    let verdict = if atlas::diverges(p, 0.40) {
                        "DIVERGE → manomesso (A3/C)"
                    } else {
                        "ok"
                    };
                    println!(
                        "  pag {} | similarità estratto<->OCR = {:.2} -> {verdict}",
                        p.page, p.similarity
                    );
                    println!("     estratto: {:?}", snippet(&p.extracted));
                    println!("     OCR:      {:?}", snippet(&p.visual));
                    // Significant word substitutions (OCR noise filtered out via edit distance):
                    // a non-empty result is semantic replacement (variant A3) — extracted != visible.
                    let subs = atlas::significant_substitutions(&p.extracted, &p.visual);
                    if !subs.is_empty() {
                        println!("     >>> SOSTITUZIONE SEMANTICA (A3): estratto != visibile");
                        for (e, v) in subs.iter().take(12) {
                            println!("         estratto {e:?}  !=  visibile {v:?}");
                        }
                    }
                }
            }
            Err(e) => println!("  errore: {e}"),
        }
    }
    Ok(())
}
