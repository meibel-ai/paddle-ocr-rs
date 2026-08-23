//! The OCR branch: reading a raster page and handing back the same [`Line`]s
//! the native branch produces, so everything downstream — columns, assembly,
//! Markdown — is shared instead of duplicated.
//!
//! Two engines answer the same call: Tesseract, through the author's
//! `tesseract5-rs`, and PP-OCRv6, through the author's `paddle-ocr-rs` fork.
//! The per-page switch between them is Phase 4's `policy`; here they are
//! selectable, so the benchmark can measure one against the other.

use std::path::PathBuf;

use image::RgbImage;
#[cfg(feature = "tesseract")]
use tesseract5_rs::{Ocr5Engine, OcrOptions};

use crate::geometry::Rect;
use crate::native::{Line, Style, Word};

/// Words below this confidence are noise more often than text. Tesseract's
/// own scale: genuine words sit above 80, garbage under 50 (measured across
/// `old_project/edito-ocr-v6`'s arbitration work, `arbitrate.py`).
const MIN_WORD_CONFIDENCE: f32 = 30.0;

#[cfg(feature = "tesseract")]
/// A Tesseract engine bound to one language set, reused across pages.
pub struct TesseractEngine {
    engine: Ocr5Engine,
}

#[cfg(feature = "tesseract")]
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

#[cfg(feature = "tesseract")]
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

#[cfg(feature = "tesseract")]
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


/// Which PP-OCRv6 model tier to load. The tiers trade accuracy for speed and
/// size (medium 134 MB, small 30 MB, tiny 6 MB); the benchmark exists to say
/// how much accuracy each step down actually costs.
#[cfg(feature = "ppocr")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaddleTier {
    Medium,
    Small,
    Tiny,
}

#[cfg(feature = "ppocr")]
impl PaddleTier {
    pub fn dir_name(self) -> &'static str {
        match self {
            PaddleTier::Medium => "medium",
            PaddleTier::Small => "small",
            PaddleTier::Tiny => "tiny",
        }
    }
}

/// PP-OCRv6: detection + recognition, no angle classifier — the benchmark
/// rasters are upright by construction, and page orientation belongs to the
/// pipeline, not the engine.
#[cfg(feature = "ppocr")]
pub struct PaddleEngine {
    ocr: paddle_ocr_rs::ocr_lite::OcrLite,
}

#[cfg(feature = "ppocr")]
impl PaddleEngine {
    /// Load a tier from `<models>/v6/<tier>/{det,rec}.onnx` + `dict.txt`.
    /// ort is load-dynamic: `ORT_DYLIB_PATH` must already point at the right
    /// onnxruntime for this machine.
    pub fn new(
        models: &std::path::Path,
        tier: PaddleTier,
    ) -> Result<Self, paddle_ocr_rs::ocr_error::OcrError> {
        let dir = models.join("v6").join(tier.dir_name());
        let path = |name: &str| dir.join(name).to_string_lossy().into_owned();
        let mut ocr = paddle_ocr_rs::ocr_lite::OcrLite::new();
        ocr.init_models_no_angle(&path("det.onnx"), &path("rec.onnx"), &path("dict.txt"), 4)?;
        Ok(PaddleEngine { ocr })
    }

    /// Read a page, same contract as `TesseractEngine::read_page`.
    pub fn read_page(
        &mut self,
        page: &RgbImage,
    ) -> Result<Vec<Line>, paddle_ocr_rs::ocr_error::OcrError> {
        // Detection at 1280 rather than the fork's customary 960: measured in
        // `old_project/edito-ocr-v6` (detect.py:32-44), 960 loses lines on
        // dense pages and past 1280 nothing more is found. The remaining
        // thresholds are the ones df-ocr-switcher ships with this fork.
        let result = self.ocr.detect(page, 10, 1280, 0.6, 0.3, 1.6, false, false)?;
        let height = page.height() as f32;
        Ok(result
            .text_blocks
            .iter()
            .filter(|block| !block.text.trim().is_empty())
            .map(|block| to_line(block, height))
            .collect())
    }
}

/// One detected text block — one visual line — becomes one [`Line`], its words
/// laid out proportionally along it. The proportion is an estimate, but
/// columns and ordering consume the line box, which is exact.
#[cfg(feature = "ppocr")]
fn to_line(block: &paddle_ocr_rs::ocr_result::TextBlock, page_height: f32) -> Line {
    let (mut left, mut right, mut top, mut bottom) = (f32::MAX, f32::MIN, f32::MIN, f32::MAX);
    for point in &block.box_points {
        left = left.min(point.x as f32);
        right = right.max(point.x as f32);
        // Raster y grows downward; page y grows upward.
        top = top.max(page_height - point.y as f32);
        bottom = bottom.min(page_height - point.y as f32);
    }
    let bbox = Rect::new(left, bottom, right, top);
    let style =
        Style { font: String::new(), size: bbox.height(), bold: false, monospace: false };

    let tokens: Vec<&str> = block.text.split_whitespace().collect();
    let total: usize = tokens.iter().map(|token| token.chars().count().max(1)).sum();
    let mut cursor = left;
    let words = tokens
        .iter()
        .map(|token| {
            let share = token.chars().count().max(1) as f32 / total.max(1) as f32;
            let width = bbox.width() * share;
            let word = Word {
                text: (*token).to_string(),
                bbox: Rect::new(cursor, bottom, cursor + width, top),
                style: style.clone(),
                visible: true,
            };
            cursor += width;
            word
        })
        .collect();
    Line { words, bbox }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "ppocr")]
    #[test]
    fn a_text_block_becomes_a_line_in_page_space() {
        use paddle_ocr_rs::ocr_result::{Point, TextBlock};
        let block = TextBlock {
            box_points: vec![
                Point { x: 100, y: 50 },
                Point { x: 500, y: 50 },
                Point { x: 500, y: 80 },
                Point { x: 100, y: 80 },
            ],
            box_score: 0.9,
            angle_index: 0,
            angle_score: 0.0,
            text: "due parole".into(),
            text_score: 0.95,
            words: Vec::new(),
        };
        let line = to_line(&block, 1000.0);
        assert_eq!(line.bbox, Rect::new(100.0, 920.0, 500.0, 950.0));
        assert_eq!(line.words.len(), 2);
        assert!(line.words[0].bbox.right <= line.words[1].bbox.left + 0.01);
    }

    #[cfg(feature = "tesseract")]
    #[test]
    fn tesseract_boxes_are_flipped_into_page_space() {
        // A word near the top of a 1000 px page: high y after the flip.
        let near_top = tesseract5_rs::BoundingBox::from_lrtb(100, 50, 300, 80);
        let flipped = flip(near_top, 1000);
        assert_eq!(flipped, Rect::new(100.0, 920.0, 300.0, 950.0));
        assert!(flipped.top > flipped.bottom, "the rect stays well-formed");
    }
}
