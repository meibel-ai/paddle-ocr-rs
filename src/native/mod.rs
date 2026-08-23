//! Phase 2 — the native branch, through pdfium.
//!
//! Text, page objects, bookmarks, metadata, annotations and images all come
//! from here (see `PLAN.md`, Phase 2).

pub mod objects;
pub mod outline;
pub mod text;

use pdfium_render::prelude::*;
use std::path::{Path, PathBuf};

pub use objects::{Annotation, ImagePlacement, Rule};
pub use outline::Bookmark;
pub use text::{Line, Style, Word};

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

/// Where ONNX Runtime lives for this platform.
///
/// ort opens it by `ORT_DYLIB_PATH`, and pointing that at the wrong
/// architecture fails with a bad-format error that says nothing useful — so
/// the path is derived from the same place pdfium's is, and never typed twice.
pub fn onnxruntime_path() -> PathBuf {
    let name = if cfg!(target_os = "windows") {
        "onnxruntime.dll"
    } else if cfg!(target_os = "macos") {
        "libonnxruntime.dylib"
    } else {
        "libonnxruntime.so"
    };
    native_dir().join(name)
}

/// Bind pdfium-render to `native/<arch>/pdfium.dll`.
pub fn bind_pdfium() -> Result<Pdfium, PdfiumError> {
    let library = Pdfium::pdfium_platform_library_name_at_path(&native_dir());
    Ok(Pdfium::new(Pdfium::bind_to_library(library)?))
}

/// Render a page to a raster at the given resolution.
///
/// The scale is `dpi / 72` because a PDF point *is* 1/72 inch, so the two
/// resolutions never have to be reconciled anywhere else: whoever renders also
/// knows how to map a pixel back to a point.
pub fn render_page(page: &PdfPage, dpi: f32) -> Result<image::RgbImage, PdfiumError> {
    let config = PdfRenderConfig::new().scale_page_by_factor(dpi / 72.0);
    Ok(page.render_with_config(&config)?.as_image().into_rgb8())
}

/// The lines of every page of a document, in pdfium's order.
///
/// A page that cannot be read yields an empty vector rather than failing the
/// document: one broken page must not cost the other three hundred.
pub fn document_lines(pdfium: &Pdfium, path: &Path) -> Result<Vec<Vec<Line>>, PdfiumError> {
    let document = pdfium.load_pdf_from_file(path, None)?;
    Ok(document.pages().iter().map(|page| text::page_lines(&page).unwrap_or_default()).collect())
}
