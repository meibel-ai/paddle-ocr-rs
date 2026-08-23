//! Document integrity — is the extracted text telling the truth?
//!
//! Two threat models, both from `chk_defaced` (see `README_CHK_DOCUMENT.md`):
//!
//! * **font defacing** — the extracted characters differ from the glyphs that
//!   are drawn, so the native reader is lying and the page has to be read from
//!   its pixels instead: those pages are routed to OCR as [`OcrReason::Garbled`];
//! * **hidden text** — text that is extractable but invisible to a reader, the
//!   prompt-injection vector. That text is *readable*, so it is not an OCR
//!   reason: it travels with the result as a warning, and later phases put it
//!   in the Markdown marked rather than dropping it silently.

use anyhow::Result;
use chk_defaced::finding::Report;
use chk_defaced::registry::FontRegistry;
use lopdf::Document;

use crate::detect::{DocumentReport, OcrReason};

/// The pipeline depends on this question, not on who answers it.
pub trait IntegrityCheck {
    /// Inspect an already-parsed document. `label` names it in the report.
    fn inspect(&self, doc: &Document, label: &str) -> Result<Report>;
}

/// The default check. Holding an optional [`FontRegistry`] is what upgrades a
/// suspected substitution to a confirmed one with a known direction.
#[derive(Default)]
pub struct ChkDefaced {
    pub registry: Option<FontRegistry>,
}

impl IntegrityCheck for ChkDefaced {
    fn inspect(&self, doc: &Document, label: &str) -> Result<Report> {
        // Single parse: this crate and chk_defaced share one lopdf version, so
        // the document loaded for detection is the one inspected here.
        chk_defaced::scan::scan_document(doc, label, self.registry.as_ref())
    }
}

/// Send the pages a defacement makes untrustworthy to OCR, and report how many
/// were routed.
///
/// The affected sentences carry their page number, so only those pages pay for
/// OCR. When a finding cannot name its pages — `PDF.TOUNICODE_GARBLED` names a
/// font, not a location — every page whose text would otherwise be trusted is
/// routed: an unlocated defacement is not evidence that the rest is clean.
/// A searchable scan counts, because a garbled OCR layer is exactly the case
/// where the pixels must be read again.
pub fn route_defaced_pages(report: &Report, pages: &mut DocumentReport) -> usize {
    if !report.assessment.is_some_and(|a| a.defaced) {
        return 0;
    }
    let affected: Vec<u32> = report.phrases.iter().filter_map(|p| p.page).collect();
    let targets: Vec<u32> = if affected.is_empty() {
        pages.pages.iter().filter(|p| p.trusts_native_text()).map(|p| p.number).collect()
    } else {
        affected
    };

    let mut routed = 0;
    for number in targets {
        if let Some(page) = pages.page_mut(number) {
            if page.ocr.is_none() {
                routed += 1;
            }
            page.require_ocr(OcrReason::Garbled);
        }
    }
    routed
}

#[cfg(test)]
mod tests {
    use chk_defaced::finding::{Assessment, PhraseDiff, Severity};

    use super::*;
    use crate::detect::{PageKind, PageReport, PageSignals};

    /// One page per `(kind, extractable bytes)` pair, none routed to OCR yet.
    fn document(pages: &[(PageKind, usize)]) -> DocumentReport {
        DocumentReport {
            pages: pages
                .iter()
                .enumerate()
                .map(|(i, &(kind, bytes))| PageReport {
                    number: i as u32 + 1,
                    kind,
                    ocr: None,
                    signals: PageSignals { visible_bytes: bytes, ..PageSignals::default() },
                })
                .collect(),
            total_pages: pages.len(),
        }
    }

    fn report(defaced: bool, pages: &[Option<u32>]) -> Report {
        let mut report = Report::new("t.pdf", "pdf");
        report.assessment = Some(Assessment {
            ok: !defaced,
            defaced,
            hidden_text: false,
            max_severity: defaced.then_some(Severity::High),
        });
        report.phrases = pages
            .iter()
            .map(|&page| PhraseDiff {
                extracted: "rn".into(),
                presumed: "m".into(),
                page,
                ocr: None,
            })
            .collect();
        report
    }

    #[test]
    fn a_clean_report_routes_nothing() {
        let mut pages = document(&[(PageKind::Digital, 500), (PageKind::Digital, 500)]);
        assert_eq!(route_defaced_pages(&report(false, &[]), &mut pages), 0);
        assert_eq!(pages.pages_needing_ocr().count(), 0);
    }

    #[test]
    fn only_the_affected_pages_pay_for_ocr() {
        let mut pages = document(&[
            (PageKind::Digital, 500),
            (PageKind::Digital, 500),
            (PageKind::Digital, 500),
        ]);
        assert_eq!(route_defaced_pages(&report(true, &[Some(2)]), &mut pages), 1);
        let routed: Vec<u32> = pages.pages_needing_ocr().map(|p| p.number).collect();
        assert_eq!(routed, [2]);
        assert_eq!(pages.page_mut(2).unwrap().ocr, Some(OcrReason::Garbled));
    }

    #[test]
    fn an_unlocated_defacement_routes_every_page_whose_text_is_trusted() {
        // A searchable scan is in: `PDF.TOUNICODE_GARBLED` on the OCR layer of
        // a scan (font `HiddenHorzOCR`, seen on a real paper) means the layer
        // lies, and the pixels have to be read again. A page with no text has
        // nothing to distrust.
        let mut pages = document(&[
            (PageKind::Digital, 500),
            (PageKind::Scanned, 800), // searchable scan: its layer is native text
            (PageKind::Mixed, 500),
            (PageKind::Empty, 0),
        ]);
        assert_eq!(route_defaced_pages(&report(true, &[None]), &mut pages), 3);
        let routed: Vec<u32> = pages.pages_needing_ocr().map(|p| p.number).collect();
        assert_eq!(routed, [1, 2, 3]);
    }
}
