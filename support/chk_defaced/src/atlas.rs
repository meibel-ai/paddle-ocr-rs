//! OCR-atlas escalation for PDF (feature `ocr-atlas`): render each page with **pdfium**, OCR the
//! rendering (what a human *sees*), and compare it with the PDF text layer (what a machine
//! *extracts*). A large divergence means the rendered glyphs do not match the extracted text —
//! semantic replacement / forged-font tampering (variant A3/C) that the deterministic checks cannot
//! catch.
//!
//! Needs the native OCR backend (`ocr-tesseract`), the pdfium dynamic library, and `tessdata`.

use std::path::Path;

use anyhow::{anyhow, Result};
use pdfium_render::prelude::*;

use crate::finding::{Category, Finding, Report, Severity, Verdict};
use crate::ocr::{GrayImage, OcrEngine, OcrHint};
use crate::textdiff::{jaccard, words};
// Re-exported for the `atlas` / `verify` examples and external callers that drove the PDF flow.
pub use crate::textdiff::significant_substitutions;

/// Per-page comparison of extracted text vs OCR of the rendered page.
pub struct PageDivergence {
    pub page: usize,
    pub extracted: String,
    pub visual: String,
    /// Word-set Jaccard similarity in 0..=1 (1 = identical word sets).
    pub similarity: f32,
}

/// Render up to `max_pages` pages at `scale`× (1.0 ≈ 72 DPI), OCR each, and compare with the page's
/// extracted text layer. `pdfium_dir` must contain the pdfium dynamic library.
pub fn ocr_vs_extracted(
    pdf: &Path,
    pdfium_dir: &Path,
    ocr: &dyn OcrEngine,
    scale: f32,
    max_pages: usize,
) -> Result<Vec<PageDivergence>> {
    let lib = Pdfium::pdfium_platform_library_name_at_path(pdfium_dir);
    let bindings =
        Pdfium::bind_to_library(&lib).map_err(|e| anyhow!("bind pdfium ({}): {e}", lib.display()))?;
    let pdfium = Pdfium::new(bindings);
    let doc = pdfium.load_pdf_from_file(pdf, None).map_err(|e| anyhow!("open pdf: {e}"))?;

    let mut out = Vec::new();
    for (i, page) in doc.pages().iter().enumerate() {
        if i >= max_pages {
            break;
        }
        let w_pt = page.width().value;
        let target_w = ((w_pt * scale).round() as i32).max(1);
        let config = PdfRenderConfig::new().set_target_width(target_w);
        let bitmap = page.render_with_config(&config).map_err(|e| anyhow!("render page: {e}"))?;
        let luma = bitmap.as_image().to_luma8();
        let (w, h) = (luma.width(), luma.height());
        let img = GrayImage::new(w, h, luma.into_raw());

        let visual = ocr.recognize(&img, OcrHint::Block)?;
        let extracted = page.text().map(|t| t.all()).unwrap_or_default();
        let similarity = jaccard(&extracted, &visual);
        out.push(PageDivergence { page: i + 1, extracted, visual, similarity });
    }
    Ok(out)
}

/// The **mandatory render-level confirmation** of a semantic-replacement (collision) signal: render +
/// OCR the pages, then (a) set the report `verdict` and adjust severity, and (b) fill each phrase's
/// `ocr` ground truth. Single render pass. `pdfium_dir` must contain the pdfium dynamic library.
///
/// - real word substitutions between rendered and extracted text → **Confirmed** (the rig is fired);
/// - none, and the page OCR closely matches the extracted text → **Refuted** (loaded-but-unfired rig):
///   the `GLYPH_SEMANTIC_REPLACEMENT` findings are downgraded to `Info` so the document is not flagged;
/// - otherwise (OCR inconclusive) → left **Unconfirmed**: the deterministic High findings stand.
pub fn verify_with_render(report: &mut Report, pdf: &Path, pdfium_dir: &Path, ocr: &dyn OcrEngine) -> Result<()> {
    let has_collision = report.findings.iter().any(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT"));
    let has_hidden_candidate = report.findings.iter().any(|f| {
        matches!(f.category, Category::HiddenContent)
            && (f.rule.contains("INVISIBLE") || f.rule.contains("TINY") || f.rule.contains("OFFPAGE"))
    });
    if !has_collision && report.phrases.is_empty() && !has_hidden_candidate {
        return Ok(());
    }
    let pages = ocr_vs_extracted(pdf, pdfium_dir, ocr, 2.0, 50)?;

    // Hidden-text confirmation (the dual of the substitution diff): words in the extracted layer that do
    // not appear in the OCR of the render are invisible to a human — the render recovers *what* is hidden.
    if has_hidden_candidate {
        let mut hidden: Vec<String> = Vec::new();
        for p in &pages {
            for w in crate::textdiff::missing_words(&p.extracted, &p.visual) {
                if !hidden.contains(&w) {
                    hidden.push(w);
                }
            }
        }
        if hidden.len() >= 4 {
            let list = hidden.iter().take(20).cloned().collect::<Vec<_>>().join(" ");
            report.push(Finding::new(
                "PDF.HIDDEN_TEXT_CONFIRMED",
                Severity::High,
                Category::HiddenContent,
                "rendered page",
                format!("render-OCR confirms {} word(s) in the extract but not visible on the page: {list}", hidden.len()),
                0.85,
            ));
        }
    }

    // (b) phrase OCR ground truth
    let ocr_text: String = pages.iter().map(|p| p.visual.as_str()).collect::<Vec<_>>().join("\n");
    let ocr_sents = crate::finding::split_sentences(&ocr_text);
    for ph in &mut report.phrases {
        let best = ocr_sents.iter().max_by(|a, b| {
            let sa = jaccard(&ph.presumed, a).max(jaccard(&ph.extracted, a));
            let sb = jaccard(&ph.presumed, b).max(jaccard(&ph.extracted, b));
            sa.partial_cmp(&sb).unwrap_or(std::cmp::Ordering::Equal)
        });
        if let Some(s) = best {
            if jaccard(&ph.presumed, s).max(jaccard(&ph.extracted, s)) >= 0.4 {
                ph.ocr = Some(s.clone());
            }
        }
    }

    // (a) verdict on the collision findings
    if !has_collision {
        return Ok(());
    }
    let subs: Vec<(String, String)> =
        pages.iter().flat_map(|p| significant_substitutions(&p.extracted, &p.visual)).collect();
    let mean_sim =
        if pages.is_empty() { 0.0 } else { pages.iter().map(|p| p.similarity).sum::<f32>() / pages.len() as f32 };

    if !subs.is_empty() {
        report.verdict = Some(Verdict::Confirmed);
        let list: String =
            subs.iter().take(10).map(|(e, v)| format!("'{e}'→'{v}'")).collect::<Vec<_>>().join(", ");
        report.push(Finding::new(
            "PDF.RENDER_DIVERGENCE_CONFIRMED",
            Severity::High,
            Category::FontIntegrity,
            "rendered page",
            format!("OCR confirms the rendered text diverges from the extracted text: {list}"),
            0.9,
        ));
    } else if mean_sim >= 0.9 {
        report.verdict = Some(Verdict::Refuted);
        for f in report.findings.iter_mut().filter(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT")) {
            f.severity = Severity::Info;
            f.message.push_str(
                " — OCR-refuted: the rendered page matches the extracted text (the font carries a glyph collision but it is not used on the visible text)",
            );
        }
    } // else: inconclusive OCR → stays Unconfirmed
    Ok(())
}

/// Convenience verdict: a page diverges (likely tampered) when the rendered page has substantial text
/// (so the OCR is trustworthy) yet it barely overlaps the extracted text. This catches the case where
/// the extracted layer is garbage/PUA while the glyphs render real words.
pub fn diverges(d: &PageDivergence, threshold: f32) -> bool {
    words(&d.visual).len() >= 5 && d.similarity < threshold
}
