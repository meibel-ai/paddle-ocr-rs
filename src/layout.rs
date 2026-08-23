//! Structure and reading order from PP-DocLayoutV3.
//!
//! A digital PDF says where its glyphs are and nothing about what they mean:
//! which lines form a paragraph, which paragraph comes after which on a
//! two-column page, what is a caption and what is a footer. Geometry can guess
//! at that, and the guess is wrong exactly where documents are interesting.
//! The layout model was trained on the answer, and it returns a reading order
//! along with the regions, so it is the preferred source for both — with
//! XY-Cut kept as the fallback where the model is not available.
//!
//! The model works on a raster, so a native page is rendered once at a low
//! resolution purely for it: layout needs shapes, not legible glyphs.

use image::RgbImage;
use paddle_ocr_rs::layout::{LayoutAnalyzer, LayoutBox, LayoutClass};
use paddle_ocr_rs::ocr_error::OcrError;
use std::path::Path;

use crate::geometry::Rect;
use crate::region::{Region, RegionKind};

/// Rendering resolution for the layout pass. The model resizes its input to
/// 800 px anyway (`LAYOUT_INPUT_SIZE`), so anything finer is paid for twice
/// and thrown away; 150 DPI keeps an A4 page comfortably above that.
pub const LAYOUT_DPI: f32 = 150.0;

/// The model knows twenty-five classes; most of the distinctions it draws
/// (`Abstract` from `Text`, `FooterImage` from `Image`) do not change what
/// gets written, so they are folded into [`RegionKind`] here rather than
/// carried through the assembler.
fn kind_of(class: LayoutClass) -> RegionKind {
    use LayoutClass as C;
    match class {
        C::DocTitle => RegionKind::Title,
        C::ParagraphTitle | C::Content => RegionKind::Heading,
        C::Table => RegionKind::Table,
        C::Image | C::Chart | C::Seal => RegionKind::Figure,
        C::FigureTitle => RegionKind::Caption,
        C::DisplayFormula | C::InlineFormula | C::FormulaNumber => RegionKind::Formula,
        C::Header | C::Footer | C::HeaderImage | C::FooterImage | C::Number => {
            RegionKind::Furniture
        }
        C::AsideText | C::Footnote | C::VisionFootnote => RegionKind::Aside,
        C::Reference | C::ReferenceContent => RegionKind::Reference,
        C::Abstract | C::Algorithm | C::Text | C::VerticalText => RegionKind::Text,
    }
}

/// The layout model, loaded once and reused across pages.
pub struct LayoutModel {
    analyzer: LayoutAnalyzer,
}

impl LayoutModel {
    /// Load `PP-DocLayoutV3.onnx`. ONNX Runtime is opened dynamically, so
    /// `ORT_DYLIB_PATH` must point at `native/<arch>/onnxruntime.dll`.
    pub fn open(model: impl AsRef<Path>) -> Result<Self, OcrError> {
        Ok(LayoutModel { analyzer: LayoutAnalyzer::from_path(model)? })
    }

    /// The regions of a rendered page, in **raster pixels**, top-left origin.
    /// Use [`to_points`] to bring them back into the page's own coordinates.
    pub fn regions(&mut self, page: &RgbImage) -> Result<Vec<Region>, OcrError> {
        Ok(self.analyzer.analyze(page)?.iter().map(to_region).collect())
    }
}

fn to_region(found: &LayoutBox) -> Region {
    Region {
        bbox: Rect::new(
            found.x as f32,
            found.y as f32,
            (found.x + found.w) as f32,
            (found.y + found.h) as f32,
        ),
        kind: kind_of(found.class),
        score: found.score,
        order: u32::try_from(found.reading_order).ok(),
    }
}

/// Bring a raster box back into PDF user space.
///
/// The raster counts pixels down from the top-left; a PDF counts points up
/// from the bottom-left. Getting this backwards puts every region on the
/// wrong half of the page, which is why it lives in one tested function.
pub fn to_points(bbox: Rect, scale: f32, page_height: f32) -> Rect {
    Rect::new(
        bbox.left / scale,
        page_height - bbox.top / scale,
        bbox.right / scale,
        page_height - bbox.bottom / scale,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_raster_box_maps_back_onto_the_page() {
        // A 150 DPI raster of an A4 page: 1240 x 1754 px for 595 x 842 pt.
        let scale = LAYOUT_DPI / 72.0;
        let page_height = 842.0;

        // A band across the top of the raster is the top of the page.
        let top_band = Rect::new(0.0, 0.0, 1240.0, 100.0);
        let mapped = to_points(top_band, scale, page_height);
        assert!((mapped.top - page_height).abs() < 0.5, "the top stays at the top");
        assert!(mapped.bottom > 780.0, "and the band stays thin");

        // A band at the bottom of the raster reaches y = 0.
        let bottom_band = Rect::new(0.0, 1754.0 - 100.0, 1240.0, 1754.0);
        let mapped = to_points(bottom_band, scale, page_height);
        assert!(mapped.bottom.abs() < 0.5);
    }

    #[test]
    fn the_model_classes_fold_into_what_markdown_needs() {
        assert_eq!(kind_of(LayoutClass::Abstract), RegionKind::Text);
        assert_eq!(kind_of(LayoutClass::ParagraphTitle), RegionKind::Heading);
        assert_eq!(kind_of(LayoutClass::FooterImage), RegionKind::Furniture);
        assert_eq!(kind_of(LayoutClass::Chart), RegionKind::Figure);
        assert!(!RegionKind::Furniture.is_content());
        assert!(RegionKind::Aside.is_content(), "a footnote is content, just not body");
    }

    #[test]
    fn an_unstated_reading_order_is_absent_not_zero() {
        let unstated = LayoutBox {
            x: 0,
            y: 0,
            w: 10,
            h: 10,
            class: LayoutClass::Text,
            score: 0.9,
            reading_order: -1,
        };
        assert_eq!(to_region(&unstated).order, None);
    }
}
