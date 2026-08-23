//! What a page is made of, independent of who worked it out.
//!
//! Regions can come from three places, in this order of preference: the
//! structure tree of a tagged PDF, the layout model, or geometry. They all
//! answer the same two questions — what is this piece of the page, and when is
//! it read — so they all produce these types, and nothing downstream has to
//! know which source was available.

use crate::geometry::Rect;

/// What a region is, for the purpose of writing Markdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegionKind {
    /// The document's own title.
    Title,
    /// A section heading.
    Heading,
    /// Body text.
    Text,
    Table,
    Figure,
    /// A caption belonging to a figure or a table.
    Caption,
    Formula,
    /// Running head or foot: repeated furniture, not content.
    Furniture,
    /// Marginalia, sidebars, footnotes — read after the body beside them.
    Aside,
    /// Bibliography and reference lists.
    Reference,
}

impl RegionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            RegionKind::Title => "title",
            RegionKind::Heading => "heading",
            RegionKind::Text => "text",
            RegionKind::Table => "table",
            RegionKind::Figure => "figure",
            RegionKind::Caption => "caption",
            RegionKind::Formula => "formula",
            RegionKind::Furniture => "furniture",
            RegionKind::Aside => "aside",
            RegionKind::Reference => "reference",
        }
    }

    /// Furniture never reaches the Markdown as content.
    pub fn is_content(self) -> bool {
        self != RegionKind::Furniture
    }
}

/// One region of a page, in the page's own coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub bbox: Rect,
    pub kind: RegionKind,
    /// How sure the source is. Geometry, having no opinion, says 1.
    pub score: f32,
    /// The reading position the source stated, if it stated one.
    pub order: Option<u32>,
}

impl Region {
    /// A region geometry alone can offer: a box, and no opinion about it.
    pub fn plain(bbox: Rect, kind: RegionKind) -> Self {
        Region { bbox, kind, score: 1.0, order: None }
    }
}
