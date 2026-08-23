//! Phase 2 — the native branch, through pdfium.
//!
//! Text, page objects, bookmarks, metadata, annotations and images all come
//! from here (see `PLAN.md`, Phase 2). For now: binding and a smoke report.

use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

/// Directory holding the runtime libraries for the current architecture.
///
/// Native libraries do not understand Windows verbatim paths (`\\?\C:\…`), so
/// this stays a plain relative path unless `PDF2MD_NATIVE_DIR` overrides it.
pub fn native_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("PDF2MD_NATIVE_DIR") {
        return PathBuf::from(dir);
    }
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        _ => "x86_64",
    };
    PathBuf::from("native").join(arch)
}

/// Bind pdfium-render to `native/<arch>/pdfium.dll`.
pub fn bind_pdfium() -> Result<Pdfium, PdfiumError> {
    let library = Pdfium::pdfium_platform_library_name_at_path(&native_dir());
    Ok(Pdfium::new(Pdfium::bind_to_library(library)?))
}

/// How much text pdfium extracts from each page — the counterpart of the
/// detection signals, and the first half of the Phase 5 fusion.
pub fn extracted_chars_per_page(pdfium: &Pdfium, path: &Path) -> Result<Vec<usize>, PdfiumError> {
    let document = pdfium.load_pdf_from_file(path, None)?;
    Ok(document
        .pages()
        .iter()
        .map(|page| page.text().map(|text| text.all().chars().count()).unwrap_or(0))
        .collect())
}
