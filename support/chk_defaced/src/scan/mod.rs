//! Per-format scanner dispatch. Each backend runs **deterministic** checks (no OCR/rendering in v1):
//! coherence of embedded/referenced fonts and Unicode hygiene of the extracted text.

use std::path::Path;

use anyhow::Result;

use crate::finding::Report;
use crate::registry::FontRegistry;

pub mod docx;
#[cfg(feature = "html")]
pub mod html;
pub mod pdf;

pub fn scan_path(path: &Path, registry: Option<&FontRegistry>) -> Result<Report> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut report = match ext.as_str() {
        "pdf" => pdf::scan(path, registry),
        "docx" => docx::scan(path, registry),
        #[cfg(feature = "html")]
        "html" | "htm" => html::scan(path, registry),
        #[cfg(not(feature = "html"))]
        "html" | "htm" => anyhow::bail!("HTML support is not compiled (enable the `html` feature)"),
        other => anyhow::bail!("unsupported format: .{other} (supported: pdf, docx, html)"),
    }?;
    report.finalize(); // explicit document-level assessment (recomputed after any OCR escalation)
    Ok(report)
}

/// Deterministic scan, then **escalate to specimen-OCR** for the residual case the outline
/// cross-reference cannot decide: a custom font with no honest anchor. The escalation runs only when the
/// deterministic pass found **no** semantic-replacement finding (so it never duplicates an already-caught
/// file and never wastes OCR on it) and only for OCR-able formats (pdf/docx). OCR errors are swallowed —
/// a missing tessdata directory degrades gracefully to the deterministic result.
#[cfg(feature = "ocr-specimen")]
pub fn scan_path_with_ocr(
    path: &Path,
    registry: Option<&FontRegistry>,
    ocr: &dyn crate::ocr::OcrEngine,
) -> Result<Report> {
    let mut report = scan_path(path, registry)?;

    let already = report.findings.iter().any(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT"));
    let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    if !already && matches!(ext.as_str(), "pdf" | "docx") {
        if let Ok(extra) = crate::specimen::specimen_scan_path(path, ocr) {
            report.findings.extend(extra);
        }
    }
    report.finalize(); // refresh the assessment after the specimen-OCR findings
    Ok(report)
}

/// Symbol/dingbat families that legitimately map into the PUA (so PUA is not a false positive).
fn is_symbol_family(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    ["wingding", "webding", "symbol", "dingbat", "marlett", "bookshelf", "mt extra"]
        .iter()
        .any(|s| n.contains(s))
}

/// Judge an **embedded** font CONSERVATIVELY (v1, no outline check).
///
/// Honesty note: a font embedded in PDF/DOCX is almost always a **subset** — its name table and
/// internal cmap may be legitimately absent/reduced, and glyph ids are renumbered. So a cmap mismatch
/// against the registry does NOT prove tampering (it false-positives on every subset). The correct,
/// false-positive-free check is **outline-vs-codepoint** (compare the outline of the glyph drawn for a
/// codepoint with the canonical outline of that same codepoint in the official font), not yet
/// implemented in this v1. Here we only emit: (a) font binary-identical to a system one → no finding;
/// (b) heavy PUA in the internal cmap of a NON-symbol font with a real cmap → obfuscation (low FP).
pub(crate) fn judge_embedded_font(
    f: &crate::font::ParsedFont,
    location: &str,
    registry: Option<&FontRegistry>,
    out: &mut Vec<crate::finding::Finding>,
) {
    use crate::finding::{Category, Finding, Severity};
    use crate::registry::Identification;

    // Font binary-identical to a system one → pristine, nothing suspicious.
    if let Some(reg) = registry {
        if reg.identify(f) == Identification::Pristine {
            return;
        }
    }

    // PUA in the internal cmap is an obfuscation signal ONLY if the font has a real cmap and is
    // NOT a symbol font (Wingdings/Symbol use the PUA legitimately). PDF subsets often have an empty
    // cmap → skipped.
    let is_symbol = f
        .family
        .as_deref()
        .or(f.full_name.as_deref())
        .or(f.postscript_name.as_deref())
        .map(is_symbol_family)
        .unwrap_or(false);
    let pua = f.pua_ratio();
    if !is_symbol && f.cmap.len() >= 16 && pua > 0.5 {
        out.push(Finding::new(
            "FONT.PUA_CMAP",
            Severity::High,
            Category::FontIntegrity,
            location,
            format!(
                "internal cmap maps {:.0}% of codepoints into the Private Use Area on a non-symbol font: likely obfuscation (extracted text unreadable)",
                pua * 100.0
            ),
            0.75,
        ));
    }
    // NB: the authoritative coherence verdict (drawn outline == canonical outline for the claimed
    // codepoint) requires comparing outlines against the canonical font → next phase.
}
