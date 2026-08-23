//! The OCR branch: reading a raster page and handing back the same [`Line`]s
//! the native branch produces, so everything downstream — columns, assembly,
//! Markdown — is shared instead of duplicated.
//!
//! This first engine is Tesseract, driven through the author's
//! `tesseract5-rs`. The PP-OCRv6 engine slots in beside it later; the
//! per-page switch between them is Phase 4's `policy`.

use std::path::PathBuf;

use image::RgbImage;
use tesseract5_rs::{Ocr5Engine, OcrOptions};

use crate::geometry::Rect;
use crate::native::{Line, Style, Word};

/// Words below this confidence are noise more often than text. Tesseract's
/// own scale: genuine words sit above 80, garbage under 50 (measured across
/// `old_project/edito-ocr-v6`'s arbitration work, `arbitrate.py`).
const MIN_WORD_CONFIDENCE: f32 = 30.0;

/// A Tesseract engine bound to one language set, reused across pages.
pub struct TesseractEngine {
    engine: Ocr5Engine,
}

impl TesseractEngine {
    /// Initialise with the given languages (`"ita+eng"` style) and tessdata
    /// directory (`models/tesseract/tessdata` in this repo's layout).
    pub fn new(lang: &str, tessdata: PathBuf) -> Result<Self, tesseract5_rs::TesseractError> {
        let engine = Ocr5Engine::new(OcrOptions {
            lang: lang.to_string(),
            psm: None, // PSM_AUTO: Tesseract does its own page segmentation
            tessdata_dir: Some(tessdata),
            with_hierarchy: true,
        })?;
        Ok(TesseractEngine { engine })
    }

    /// Read a page. The lines come back in the page's own coordinate space —
    /// origin bottom-left, like pdfium's — so the caller cannot tell which
    /// branch produced them.
    pub fn read_page(&self, page: &RgbImage) -> Result<Vec<Line>, tesseract5_rs::TesseractError> {
        let (width, height) = (page.width() as i32, page.height() as i32);
        let output = self.engine.recognize(page.as_raw(), width, height, 3, width * 3)?;
        let Some(hierarchy) = output.hierarchy else { return Ok(Vec::new()) };

        let mut lines = Vec::new();
        for block in &hierarchy.blocks {
            for paragraph in &block.paragraphs {
                for text_line in &paragraph.lines {
                    let words: Vec<Word> = text_line
                        .words
                        .iter()
                        .filter(|word| {
                            word.confidence >= MIN_WORD_CONFIDENCE
                                && !word.text.trim().is_empty()
                        })
                        .map(|word| Word {
                            text: word.text.trim().to_string(),
                            bbox: flip(word.bbox, height),
                            style: style_of(word.bbox),
                            visible: true,
                        })
                        .collect();
                    if !words.is_empty() {
                        let bbox = words.iter().map(|word| word.bbox).collect();
                        lines.push(Line { words, bbox });
                    }
                }
            }
        }
        Ok(lines)
    }
}

/// Tesseract counts pixels down from the top-left; the rest of the pipeline
/// counts up from the bottom-left.
fn flip(bbox: tesseract5_rs::BoundingBox, page_height: i32) -> Rect {
    Rect::new(
        bbox.left as f32,
        (page_height - bbox.bottom) as f32,
        bbox.right as f32,
        (page_height - bbox.top) as f32,
    )
}

/// OCR knows no fonts: the size is the box height, which is what the heading
/// tiers and the clustering actually consume.
fn style_of(bbox: tesseract5_rs::BoundingBox) -> Style {
    Style {
        font: String::new(),
        size: (bbox.bottom - bbox.top) as f32,
        bold: false,
        monospace: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tesseract_boxes_are_flipped_into_page_space() {
        // A word near the top of a 1000 px page: high y after the flip.
        let near_top = tesseract5_rs::BoundingBox::from_lrtb(100, 50, 300, 80);
        let flipped = flip(near_top, 1000);
        assert_eq!(flipped, Rect::new(100.0, 920.0, 300.0, 950.0));
        assert!(flipped.top > flipped.bottom, "the rect stays well-formed");
    }
}
