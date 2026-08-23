//! Glyph-atlas detector demo (feature `ocr-atlas`): runs `chk_defaced::glyph::glyph_scan`, which
//! clusters glyphs by image, flags glyphs extracted as more than one letter, and OCRs ONLY those
//! ambiguous glyphs to resolve the truth and pinpoint the lying extractions.
//!
//! Run: cargo run --example glyphmap --features ocr-atlas -- <pdf>...
//! Env: PDFIUM_DIR, TESSDATA_DIR, ATLAS_MAX_PAGES

use std::path::{Path, PathBuf};
use std::time::Instant;

use chk_defaced::glyph;
use chk_defaced::ocr::{OcrHint, TesseractOcr};

fn main() -> anyhow::Result<()> {
    let pdfium_dir = std::env::var("PDFIUM_DIR")
        .unwrap_or_else(|_| r"C:\Progetti\pageindex-rs\native\arm64".to_string());
    let tessdata = std::env::var("TESSDATA_DIR").ok().map(PathBuf::from);
    let max_pages: usize =
        std::env::var("ATLAS_MAX_PAGES").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    // OCR languages (Tesseract `+`-joined, e.g. "ita+eng"); override with OCR_LANG.
    let lang = std::env::var("OCR_LANG").unwrap_or_else(|_| "ita+eng".to_string());
    let ocr = TesseractOcr::new(tessdata, &lang, OcrHint::SingleChar)?;

    for arg in std::env::args().skip(1) {
        let pdf = PathBuf::from(&arg);
        let name = pdf.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!("\n=== {name} ===");

        let t0 = Instant::now();
        let (findings, stats) = glyph::glyph_scan(&pdf, Path::new(&pdfium_dir), &ocr, 3.0, max_pages)?;
        let t = t0.elapsed();

        println!(
            "  caratteri {} · glifi UNICI {} (dedup {:.0}×) · glifi ambigui {} · chiamate OCR {} · t = {:.1}s",
            stats.total_chars,
            stats.unique_glyphs,
            stats.total_chars as f32 / stats.unique_glyphs.max(1) as f32,
            stats.ambiguous_glyphs,
            stats.ocr_calls,
            t.as_secs_f64()
        );
        if findings.is_empty() {
            println!("  coerenza glifo<->carattere: OK");
        } else {
            for f in &findings {
                println!("   [{:?}] {} — {}", f.severity, f.rule, f.message);
            }
        }
    }
    Ok(())
}
