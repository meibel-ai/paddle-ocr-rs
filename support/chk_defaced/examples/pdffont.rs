//! PDF embedded-font semantic-replacement scan (deterministic, no OCR, no rendering, font-agnostic).
//!
//! Run: cargo run --example pdffont -- <pdf>...

use std::path::PathBuf;

use chk_defaced::pdf_glyph;

fn main() -> anyhow::Result<()> {
    for arg in std::env::args().skip(1) {
        let p = PathBuf::from(&arg);
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        println!("\n=== {name} ===");
        match pdf_glyph::pdf_outline_scan(&p) {
            Ok(scan) if scan.findings.is_empty() => {
                println!("  nessuna sostituzione semantica (outline coerenti)")
            }
            Ok(scan) => {
                for f in &scan.findings {
                    println!("   [{:?}] {} — {}", f.severity, f.rule, f.message);
                }
                if !scan.substitutions.is_empty() {
                    let m: String =
                        scan.substitutions.iter().map(|(e, c)| format!("{e}→{c}")).collect::<Vec<_>>().join("  ");
                    println!("   sostituzioni (estratto→corretto): {m}");
                }
            }
            Err(e) => println!("  errore: {e:#}"),
        }
    }
    Ok(())
}
