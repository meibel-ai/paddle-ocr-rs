//! pdf-extractor-2-md — PDF and image documents to Markdown.
//!
//! Two branches with per-page routing (see `PLAN.md`): fast detection over the
//! raw content streams decides what each page is, the native branch reads
//! digital pages through pdfium, the OCR branch reads the rest with PP-OCRv6
//! (Tesseract as the fallback engine), and the two are fused per page before
//! the Markdown is assembled.

pub mod detect;
pub mod integrity;
pub mod native;
