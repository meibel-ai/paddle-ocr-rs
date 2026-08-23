//! Two-phase verification with performance measurements.
//!
//!   Phase 1 — algorithmic (deterministic, no OCR): structural font/cmap/ToUnicode checks. Catches
//!     PUA / ToUnicode obfuscation outright. Fast.
//!   Phase 2 — OCR, only when Phase 1 is INCONCLUSIVE (forged/unknown fonts → possible semantic
//!     replacement that has no algorithmic marker). Per the rule: an unknown font ⇒ OCR all its text;
//!     specific suspect characters are already decided by Phase 1 without OCR.
//!
//! Reports, per file: suspect characters checked, compromised characters, time(Phase 1), time(Phase 2).
//!
//! Run: cargo run --example verify --features ocr-atlas -- <pdf>...
//! Env: PDFIUM_DIR, TESSDATA_DIR, ATLAS_MAX_PAGES

use std::path::{Path, PathBuf};
use std::time::Instant;

use chk_defaced::finding::Severity;
use chk_defaced::ocr::{OcrHint, TesseractOcr};
use chk_defaced::{atlas, scan};

fn main() -> anyhow::Result<()> {
    let pdfium_dir =
        std::env::var("PDFIUM_DIR").unwrap_or_else(|_| r"C:\Progetti\pageindex-rs\native\arm64".to_string());
    let tessdata = std::env::var("TESSDATA_DIR").ok().map(PathBuf::from);
    let max_pages: usize =
        std::env::var("ATLAS_MAX_PAGES").ok().and_then(|v| v.parse().ok()).unwrap_or(30);
    // OCR languages (Tesseract `+`-joined, e.g. "ita+eng"); override with OCR_LANG.
    let lang = std::env::var("OCR_LANG").unwrap_or_else(|_| "ita+eng".to_string());
    let ocr = TesseractOcr::new(tessdata, &lang, OcrHint::Block)?;

    for arg in std::env::args().skip(1) {
        let pdf = PathBuf::from(&arg);
        let name = pdf.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!("\n=== {name} ===");

        // ── PHASE 1 — algorithmic (deterministic, NO OCR) ───────────────────────────────────────
        let t0 = Instant::now();
        let report = scan::scan_path(&pdf, None)?;
        let t_algo = t0.elapsed();
        let high: Vec<_> = report.findings.iter().filter(|f| f.severity >= Severity::High).collect();
        // Characters the algorithm flags as suspect (counts embedded in the deterministic findings).
        let suspect_algo: usize = high.iter().map(|f| first_count(&f.message)).sum();
        println!(
            "  FASE 1 (algoritmica): {} font esaminati · {} anomalie High · ~{} caratteri sospetti · t = {:.1} ms",
            report.fonts_examined,
            high.len(),
            suspect_algo,
            t_algo.as_secs_f64() * 1000.0
        );
        for f in &high {
            println!("       [{:?}] {} — {}", f.severity, f.rule, f.location);
        }

        if !high.is_empty() {
            // Suspect characters identified WITHOUT OCR → all compromised. No OCR needed.
            println!(
                "  ⇒ manomissione rilevata in Fase 1: caratteri controllati ~{suspect_algo}, compromessi ~{suspect_algo}; OCR NON necessario (t_OCR = 0)."
            );
            continue;
        }

        // ── PHASE 2 — OCR (only because Phase 1 was inconclusive) ────────────────────────────────
        let t1 = Instant::now();
        let pages = atlas::ocr_vs_extracted(&pdf, Path::new(&pdfium_dir), &ocr, 2.0, max_pages)?;
        let t_ocr = t1.elapsed();

        let chars_checked: usize = pages.iter().map(|p| p.extracted.chars().count()).sum();
        let mut subs: Vec<(usize, String, String)> = Vec::new();
        for p in &pages {
            for (e, v) in atlas::significant_substitutions(&p.extracted, &p.visual) {
                subs.push((p.page, e, v));
            }
        }
        let compromised: usize = subs.iter().map(|(_, e, _)| e.chars().count()).sum();

        println!(
            "  FASE 2 (OCR mirato — font non verificabili → OCR del testo, {} pagine): {} caratteri controllati · {} compromessi · t = {:.2} s",
            pages.len(),
            chars_checked,
            compromised,
            t_ocr.as_secs_f64()
        );
        if subs.is_empty() {
            println!("       nessuna sostituzione semantica → documento pulito");
        } else {
            for (pg, e, v) in &subs {
                println!("       pag {pg}: estratto {e:?} != visibile {v:?}  ⇒ COMPROMESSO");
            }
        }
    }
    Ok(())
}

/// First integer in a finding message (e.g. "153 ToUnicode mappings…"). 0 if none.
fn first_count(s: &str) -> usize {
    let mut n = String::new();
    for c in s.chars() {
        if c.is_ascii_digit() {
            n.push(c);
        } else if !n.is_empty() {
            break;
        }
    }
    n.parse().unwrap_or(0)
}
