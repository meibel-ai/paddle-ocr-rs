//! `chk_defaced` — library API: font-coherence checks for documents (PDF/DOCX/HTML) and a
//! system-font cmap registry. Used both by the `chk_defaced` CLI and as a dependency (e.g. to run a
//! pre-extraction coherence check before ingesting a document).
//!
//! Background: "What you see is not what your AI reads"
//! <https://dariofinardi.it/what-you-see-is-not-what-your-ai-reads-c3fed388d3bc>.
//!
//! Minimal embedding example:
//! ```no_run
//! let report = chk_defaced::scan::scan_path(std::path::Path::new("contract.pdf"), None)?;
//! if report.findings.iter().any(|f| f.severity >= chk_defaced::finding::Severity::High) {
//!     eprintln!("document may be defaced: extracted text could diverge from what is rendered");
//! }
//! # Ok::<(), anyhow::Error>(())
//! ```
//!
//! The `html` feature pulls the (heavier) HTML+CSS backend; disable it (`default-features = false`)
//! when only PDF/DOCX scanning is needed.

/// One embedded font for the specimen-OCR escalation: its raw bytes and the `(claimed_char, glyph_id)`
/// pairs the document extracts. Produced by `pdf_glyph::pdf_font_claims` / `docx_glyph::docx_font_claims`,
/// consumed by `specimen::specimen_scan`.
pub type FontClaims = (Vec<u8>, Vec<(char, u16)>);

#[cfg(feature = "ocr-atlas")]
pub mod atlas;
pub mod docx_glyph;
pub mod docx_html;
pub mod finding;
pub mod pdf_glyph;
pub mod font;
pub mod glyphmatch;
pub mod metadata;
#[cfg(feature = "ocr-atlas")]
pub mod glyph;
pub mod ocr;
pub mod registry;
#[cfg(feature = "render-wry")]
pub mod render;
pub mod scan;
#[cfg(feature = "ocr-specimen")]
pub mod specimen;
pub mod textdiff;
pub mod unicode;
pub mod visibility;
