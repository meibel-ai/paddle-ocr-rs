//! Phase 1 — per-page detection: what each page is made of, which pages the
//! native reader cannot be trusted with, and why.
//!
//! This pass runs on the raw content streams (lopdf) before pdfium touches the
//! file. Two rules carried over from the reference pipelines:
//!
//! * a page is classified by its **text rendering mode**, not by the mere
//!   presence of text (`old_project/edito-ocr-v6/src/edito_ocr/quality.py`
//!   lines 180-253): an invisible `3 Tr` layer over a full-page image is a
//!   scan with an OCR layer, not a digital page;
//! * the OCR decision is **per page and typed**
//!   (`old_project/pdf-inspector/src/detector.rs`): never one boolean for the
//!   whole document, and never an unexplained one.

mod content;

use lopdf::Document;

/// Shown bytes below which a page is treated as carrying no real text. Small
/// enough to keep a sparse page (a title, a stamp), large enough to ignore the
/// stray glyphs that page furniture leaves on a scan.
const MIN_TEXT_BYTES: usize = 30;

/// Image coverage above which a page is a scan rather than an illustrated
/// page. Measured over the whole `test/` corpus (22 documents, 2026-08-23):
/// separates every scan from every digital page with no ambiguous case.
const FULL_PAGE_IMAGE: f32 = 0.85;

/// Vector-text signature: text converted to outlines draws thousands of paths
/// and extracts as almost nothing (`pdf-inspector/src/detector.rs:861-863`).
const VECTOR_MIN_PATHS: usize = 1000;
const VECTOR_PATHS_PER_TEXT_OP: usize = 200;
const VECTOR_MAX_ALNUM: usize = 30;

/// Share of pages of one kind above which the document takes that kind.
const DOCUMENT_MAJORITY: f32 = 0.9;

/// What a page is made of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageKind {
    /// Text drawn as text: the native reader owns this page.
    Digital,
    /// A page-sized image, with or without a searchable text layer over it.
    Scanned,
    /// Native text *and* a page-sized image together — the hard case, where
    /// neither reader alone is right.
    Mixed,
    /// Nothing to read.
    Empty,
}

impl PageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PageKind::Digital => "digital",
            PageKind::Scanned => "scanned",
            PageKind::Mixed => "mixed",
            PageKind::Empty => "empty",
        }
    }
}

/// Why a page cannot be read natively. Typed, so the OCR branch can act on the
/// cause instead of guessing, and so a report can explain itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OcrReason {
    /// A page-sized image with no usable text layer.
    Scanned,
    /// Drawings but no text at all.
    NoText,
    /// Text converted to outlines: visible to a human, absent from extraction.
    VectorText,
    /// Text is extractable but untrustworthy — a defaced font makes the
    /// extracted characters differ from the drawn glyphs
    /// (set by [`crate::integrity`], not by this pass).
    Garbled,
}

impl OcrReason {
    pub fn as_str(self) -> &'static str {
        match self {
            OcrReason::Scanned => "scanned",
            OcrReason::NoText => "no_text",
            OcrReason::VectorText => "vector_text",
            OcrReason::Garbled => "garbled",
        }
    }
}

/// What one page's content stream revealed.
#[derive(Debug, Default, Clone, Copy)]
pub struct PageSignals {
    /// Text-showing operators (`Tj`, `TJ`, `'`, `"`).
    pub text_ops: usize,
    /// Bytes shown in a visible rendering mode.
    pub visible_bytes: usize,
    /// Bytes shown in rendering mode 3 or 7 — an OCR layer, or hidden text.
    pub invisible_bytes: usize,
    /// Distinct ASCII alphanumerics among the shown bytes.
    pub distinct_alnum: usize,
    /// Path-construction operators, the vector-text tell-tale.
    pub path_ops: usize,
    /// `Tf` operators, a proxy for typographic variety.
    pub font_changes: usize,
    pub image_count: usize,
    /// Share of the page area covered by placed images, clamped to 1.
    pub image_coverage: f32,
    pub content_len: usize,
}

impl PageSignals {
    /// All shown bytes, visible or not.
    pub fn shown_bytes(&self) -> usize {
        self.visible_bytes + self.invisible_bytes
    }

    fn has_visible_text(&self) -> bool {
        self.visible_bytes >= MIN_TEXT_BYTES
    }

    /// Enough text to read, whether it is painted or an invisible layer.
    pub fn has_extractable_text(&self) -> bool {
        self.shown_bytes() >= MIN_TEXT_BYTES
    }

    fn is_vector_text(&self) -> bool {
        self.path_ops >= VECTOR_MIN_PATHS
            && self.path_ops > self.text_ops.saturating_mul(VECTOR_PATHS_PER_TEXT_OP)
            && self.distinct_alnum < VECTOR_MAX_ALNUM
    }
}

/// One page's verdict.
#[derive(Debug, Clone, Copy)]
pub struct PageReport {
    /// 1-based page number, as a reader counts pages.
    pub number: u32,
    pub kind: PageKind,
    /// Why this page needs OCR, or `None` if the native reader suffices.
    pub ocr: Option<OcrReason>,
    pub signals: PageSignals,
}

impl PageReport {
    /// A scan that already carries a searchable text layer: its text is
    /// extractable, and Phase 5 decides whether to trust it.
    pub fn has_text_layer(&self) -> bool {
        self.kind == PageKind::Scanned && self.signals.has_extractable_text()
    }

    /// The pipeline would read this page's text natively: it has text and
    /// nothing has ruled it out yet. These are the pages an integrity finding
    /// can still take away — a searchable scan included, since its OCR layer
    /// is native text like any other.
    pub fn trusts_native_text(&self) -> bool {
        self.ocr.is_none() && self.signals.has_extractable_text()
    }

    /// Route this page to OCR, keeping the reason already recorded — the first
    /// cause found is the more specific one.
    pub fn require_ocr(&mut self, reason: OcrReason) {
        self.ocr.get_or_insert(reason);
    }
}

/// Which pages to look at. Sampling answers "what is this document?" cheaply;
/// the full scan is what the extraction pipeline routes on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ScanStrategy {
    #[default]
    Full,
    /// At most `n` pages, evenly spread, first and last always included — the
    /// two that differ most from the body of a document.
    Sample(usize),
}

impl ScanStrategy {
    /// 0-based page indices to scan, ascending.
    fn select(self, total: usize) -> Vec<usize> {
        match self {
            ScanStrategy::Full => (0..total).collect(),
            ScanStrategy::Sample(n) if n == 0 || total == 0 => Vec::new(),
            ScanStrategy::Sample(n) if n >= total => (0..total).collect(),
            ScanStrategy::Sample(1) => vec![0],
            ScanStrategy::Sample(n) => {
                let last = total - 1;
                let step = last as f64 / (n - 1) as f64;
                let mut pages: Vec<usize> =
                    (0..n).map(|i| (i as f64 * step).round() as usize).collect();
                pages.dedup();
                pages
            }
        }
    }
}

/// The document's verdict: one entry per scanned page.
#[derive(Debug, Clone)]
pub struct DocumentReport {
    pub pages: Vec<PageReport>,
    /// Pages in the document, which exceeds `pages.len()` when sampling.
    pub total_pages: usize,
}

impl DocumentReport {
    /// The document's kind, from the pages that carry content: one kind needs
    /// a large majority to name the whole document, otherwise it is `Mixed`.
    pub fn kind(&self) -> PageKind {
        let with_content: Vec<PageKind> =
            self.pages.iter().map(|p| p.kind).filter(|&k| k != PageKind::Empty).collect();
        if with_content.is_empty() {
            return PageKind::Empty;
        }
        let share = |kind: PageKind| {
            with_content.iter().filter(|&&k| k == kind).count() as f32 / with_content.len() as f32
        };
        if share(PageKind::Digital) >= DOCUMENT_MAJORITY {
            PageKind::Digital
        } else if share(PageKind::Scanned) >= DOCUMENT_MAJORITY {
            PageKind::Scanned
        } else {
            PageKind::Mixed
        }
    }

    pub fn pages_needing_ocr(&self) -> impl Iterator<Item = &PageReport> {
        self.pages.iter().filter(|p| p.ocr.is_some())
    }

    /// Mutable access to a page by its 1-based number.
    pub fn page_mut(&mut self, number: u32) -> Option<&mut PageReport> {
        self.pages.iter_mut().find(|p| p.number == number)
    }
}

/// Classify a document, page by page.
pub fn scan_document(doc: &Document, strategy: ScanStrategy) -> DocumentReport {
    let page_ids: Vec<(u32, lopdf::ObjectId)> = doc.get_pages().into_iter().collect();
    let pages = strategy
        .select(page_ids.len())
        .into_iter()
        .filter_map(|index| page_ids.get(index))
        .map(|&(number, id)| {
            let signals = content::page_signals(doc, id);
            let (kind, ocr) = classify(&signals);
            PageReport { number, kind, ocr, signals }
        })
        .collect();
    DocumentReport { pages, total_pages: page_ids.len() }
}

/// The classification rules, kept in one place so they read as a whole.
fn classify(signals: &PageSignals) -> (PageKind, Option<OcrReason>) {
    if signals.image_coverage >= FULL_PAGE_IMAGE {
        // A page-sized image. Visible text on top of it is native content the
        // scan cannot account for, which is what makes the page mixed.
        if signals.has_visible_text() {
            return (PageKind::Mixed, None);
        }
        let reason = (!signals.has_extractable_text()).then_some(OcrReason::Scanned);
        return (PageKind::Scanned, reason);
    }
    if signals.is_vector_text() {
        // Outlined text: the page is drawn, not scanned, but only OCR can read it.
        return (PageKind::Digital, Some(OcrReason::VectorText));
    }
    if signals.has_extractable_text() {
        return (PageKind::Digital, None);
    }
    if signals.image_count > 0 {
        return (PageKind::Scanned, Some(OcrReason::Scanned));
    }
    if signals.path_ops > 0 {
        // Drawings only: OCR may still find labels inside them.
        return (PageKind::Digital, Some(OcrReason::NoText));
    }
    (PageKind::Empty, None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signals(visible: usize, invisible: usize, coverage: f32) -> PageSignals {
        PageSignals {
            text_ops: if visible + invisible > 0 { 1 } else { 0 },
            visible_bytes: visible,
            invisible_bytes: invisible,
            distinct_alnum: 20,
            image_count: if coverage > 0.0 { 1 } else { 0 },
            image_coverage: coverage,
            ..PageSignals::default()
        }
    }

    #[test]
    fn full_page_image_without_text_is_a_scan_to_ocr() {
        let (kind, ocr) = classify(&signals(0, 0, 0.95));
        assert_eq!(kind, PageKind::Scanned);
        assert_eq!(ocr, Some(OcrReason::Scanned));
    }

    #[test]
    fn invisible_layer_over_an_image_is_a_scan_that_needs_no_ocr() {
        // The rendering-mode rule: extractable text over a full-page image is
        // a searchable-scan layer, not a digital page.
        let (kind, ocr) = classify(&signals(0, 400, 0.95));
        assert_eq!(kind, PageKind::Scanned);
        assert_eq!(ocr, None);
    }

    #[test]
    fn visible_text_over_a_full_page_image_is_mixed() {
        let (kind, ocr) = classify(&signals(400, 0, 0.95));
        assert_eq!(kind, PageKind::Mixed);
        assert_eq!(ocr, None);
    }

    #[test]
    fn outlined_text_is_digital_but_unreadable() {
        let outlined = PageSignals {
            path_ops: 5_000,
            distinct_alnum: 2,
            visible_bytes: 4,
            ..PageSignals::default()
        };
        assert_eq!(classify(&outlined), (PageKind::Digital, Some(OcrReason::VectorText)));
    }

    #[test]
    fn a_dense_text_page_is_never_mistaken_for_vector_text() {
        let prose = PageSignals {
            text_ops: 300,
            visible_bytes: 4_000,
            distinct_alnum: 45,
            path_ops: 1_200, // a page can rule tables and still be prose
            ..PageSignals::default()
        };
        assert_eq!(classify(&prose), (PageKind::Digital, None));
    }

    #[test]
    fn an_empty_page_asks_for_no_ocr() {
        assert_eq!(classify(&PageSignals::default()), (PageKind::Empty, None));
    }

    #[test]
    fn sampling_always_includes_the_first_and_last_page() {
        assert_eq!(ScanStrategy::Sample(4).select(100), [0, 33, 66, 99]);
        assert_eq!(ScanStrategy::Sample(8).select(3), [0, 1, 2]);
        assert_eq!(ScanStrategy::Sample(2).select(2), [0, 1]);
        assert!(ScanStrategy::Sample(3).select(0).is_empty());
    }

    #[test]
    fn document_kind_needs_a_majority_and_ignores_empty_pages() {
        let page = |number, kind| PageReport {
            number,
            kind,
            ocr: None,
            signals: PageSignals::default(),
        };
        let mostly_digital = DocumentReport {
            pages: vec![
                page(1, PageKind::Digital),
                page(2, PageKind::Digital),
                page(3, PageKind::Empty),
            ],
            total_pages: 3,
        };
        assert_eq!(mostly_digital.kind(), PageKind::Digital);

        let half_and_half = DocumentReport {
            pages: vec![page(1, PageKind::Digital), page(2, PageKind::Scanned)],
            total_pages: 2,
        };
        assert_eq!(half_and_half.kind(), PageKind::Mixed);
    }

    #[test]
    fn the_first_ocr_reason_recorded_wins() {
        let mut report =
            PageReport { number: 1, kind: PageKind::Digital, ocr: None, signals: PageSignals::default() };
        report.require_ocr(OcrReason::VectorText);
        report.require_ocr(OcrReason::Garbled);
        assert_eq!(report.ocr, Some(OcrReason::VectorText));
    }
}
