//! What a document says about itself: its outline and its metadata.
//!
//! The outline matters more than it looks. A PDF that carries bookmarks has
//! already been told where its headings are and how they nest, by whoever
//! wrote it — which beats inferring the hierarchy from type sizes. Where the
//! outline covers the document, it is the heading source; where it is absent
//! or partial, typography takes over (see `PLAN.md`, Phase 3).

use pdfium_render::prelude::*;

/// Nesting levels to descend. A real outline is a handful deep; a cycle or a
/// pathological document must not turn into an endless walk.
const MAX_DEPTH: usize = 8;

/// One entry of the document outline.
#[derive(Debug, Clone)]
pub struct Bookmark {
    pub title: String,
    /// Nesting level, 0 for a top-level entry.
    pub level: usize,
    /// 1-based page it points at, when it names one.
    pub page: Option<u32>,
}

/// The outline, flattened depth-first — the order a reader meets the headings.
pub fn bookmarks(document: &PdfDocument) -> Vec<Bookmark> {
    let mut flat = Vec::new();
    if let Some(root) = document.bookmarks().root() {
        for bookmark in root.iter_siblings() {
            collect(&bookmark, 0, &mut flat);
        }
    }
    flat
}

fn collect(bookmark: &PdfBookmark, level: usize, flat: &mut Vec<Bookmark>) {
    if level >= MAX_DEPTH {
        return;
    }
    if let Some(title) = bookmark.title().map(|title| title.trim().to_string()) {
        if !title.is_empty() {
            flat.push(Bookmark { title, level, page: target_page(bookmark) });
        }
    }
    for child in bookmark.iter_direct_children() {
        collect(&child, level + 1, flat);
    }
}

/// The 1-based page a bookmark points at, from its destination or its action.
fn target_page(bookmark: &PdfBookmark) -> Option<u32> {
    // The page index is read inside each branch rather than the destination
    // handed back: a destination borrows the action it came from.
    let index = match bookmark.destination().and_then(|to| to.page_index().ok()) {
        Some(index) => index,
        None => {
            let action = bookmark.action()?;
            let local = action.as_local_destination_action()?;
            local.destination().ok()?.page_index().ok()?
        }
    };
    Some(index as u32 + 1)
}

/// The `/Info` fields a document carries, as `(name, value)` pairs.
///
/// Provenance is evidence: a producer that is known to garble text, or a
/// document whose language contradicts what was read from it, changes how the
/// rest of the result should be read.
pub fn metadata(document: &PdfDocument) -> Vec<(String, String)> {
    document
        .metadata()
        .iter()
        .filter(|tag| !tag.value().trim().is_empty())
        .map(|tag| (format!("{:?}", tag.tag_type()), tag.value().to_string()))
        .collect()
}
