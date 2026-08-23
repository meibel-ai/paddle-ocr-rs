//! Pluggable OCR engine — the escalation path for the cases the deterministic checks cannot decide:
//! semantic replacement with a **forged/unknown font** (variant C), where there is no canonical
//! outline to compare against. There the only ground truth is *what the glyph actually depicts*, so we
//! rasterize the suspect glyph (or region) and OCR it, then compare the recognized text to the
//! codepoint the document claims.
//!
//! Backends sit behind a trait so the heavy/native ones are optional:
//! - [`MockOcr`] — always available, deterministic, for tests.
//! - [`TesseractOcr`] — feature `ocr-tesseract`, built on the high-level `tesseract5-rs` crate, which
//!   depends on the `tesseract-55-rs` binding (Tesseract 5.5 + Leptonica 1.85). Requires a native
//!   Tesseract build and a `tessdata` directory.
//!
//! Following the project's efficiency rule, OCR receives only **cropped bitmaps of suspect glyphs /
//! regions**, never the whole document — cost scales with the tampering, not the page count.

use anyhow::Result;

/// A grayscale bitmap, 1 byte per pixel, row-major (stride = `width`).
pub struct GrayImage {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl GrayImage {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        Self { data, width, height }
    }
}

/// Page-segmentation hint (maps to Tesseract PSM 10/8/7/6).
#[derive(Debug, Clone, Copy)]
pub enum OcrHint {
    SingleChar,
    SingleWord,
    SingleLine,
    Block,
}

/// Recognizes glyph text from a cropped bitmap. Returns plain text only (no model in the loop).
pub trait OcrEngine {
    fn recognize(&self, img: &GrayImage, hint: OcrHint) -> Result<String>;

    /// Recognize, returning each token (word) with its **confidence in `[0, 100]`**. Used by the
    /// specimen escalation to drop low-confidence reads (the source of `c/e` / `s/f` OCR confusions).
    /// Engines without real scores fall back to splitting [`recognize`](OcrEngine::recognize) at
    /// whitespace with full confidence.
    fn recognize_scored(&self, img: &GrayImage, hint: OcrHint) -> Result<Vec<(String, f32)>> {
        Ok(self.recognize(img, hint)?.split_whitespace().map(|w| (w.to_string(), 100.0)).collect())
    }
}

/// Deterministic mock for tests (no native deps): returns a scripted string regardless of input.
pub struct MockOcr(pub String);

impl OcrEngine for MockOcr {
    fn recognize(&self, _img: &GrayImage, _hint: OcrHint) -> Result<String> {
        Ok(self.0.clone())
    }
}

#[cfg(feature = "ocr-tesseract")]
pub use tesseract_backend::TesseractOcr;

#[cfg(feature = "ocr-tesseract")]
mod tesseract_backend {
    use std::path::PathBuf;

    use anyhow::{anyhow, Result};
    use tesseract5_rs::{Ocr5Engine, OcrOptions};

    use super::{GrayImage, OcrEngine, OcrHint};

    /// PSM (page-segmentation mode) for a hint. The atlas use case is a single drawn glyph → PSM 10.
    fn psm_for(hint: OcrHint) -> u8 {
        match hint {
            OcrHint::SingleChar => 10,
            OcrHint::SingleWord => 8,
            OcrHint::SingleLine => 7,
            OcrHint::Block => 6,
        }
    }

    /// Tesseract 5 backend, built on the high-level `tesseract5-rs` crate
    /// (which itself depends on the `tesseract-55-rs` binding — Tesseract 5.5 + Leptonica 1.85).
    /// The PSM is fixed at construction (mirrors `Ocr5Engine`), so the per-call hint only sets the
    /// default at `new`; build one engine per hint if you need to mix modes.
    pub struct TesseractOcr {
        engine: Ocr5Engine,
    }

    impl TesseractOcr {
        /// Initialize with an optional `tessdata` directory (else the crate default / `TESSDATA_PREFIX`),
        /// a language (e.g. `"eng"`) and a page-segmentation hint.
        pub fn new(tessdata: Option<PathBuf>, lang: &str, hint: OcrHint) -> Result<Self> {
            let engine = Ocr5Engine::new(OcrOptions {
                lang: lang.to_string(),
                psm: Some(psm_for(hint)),
                tessdata_dir: tessdata,
                with_hierarchy: true, // needed for per-word confidence (recognize_scored)
            })
            .map_err(|e| anyhow!("tesseract5 init: {e}"))?;
            Ok(Self { engine })
        }

        /// Single-glyph engine (PSM 10) with the default tessdata location — the atlas escalation default.
        pub fn for_glyph(lang: &str) -> Result<Self> {
            Self::new(None, lang, OcrHint::SingleChar)
        }
    }

    impl OcrEngine for TesseractOcr {
        fn recognize(&self, img: &GrayImage, _hint: OcrHint) -> Result<String> {
            // grayscale: 1 byte/pixel, stride = width. (PSM was fixed at construction.)
            let out = self
                .engine
                .recognize(&img.data, img.width as i32, img.height as i32, 1, img.width as i32)
                .map_err(|e| anyhow!("ocr recognize: {e}"))?;
            Ok(out.text.trim().to_string())
        }

        fn recognize_scored(&self, img: &GrayImage, _hint: OcrHint) -> Result<Vec<(String, f32)>> {
            let out = self
                .engine
                .recognize(&img.data, img.width as i32, img.height as i32, 1, img.width as i32)
                .map_err(|e| anyhow!("ocr recognize: {e}"))?;
            let mut words = Vec::new();
            if let Some(h) = out.hierarchy {
                for block in h.blocks {
                    for para in block.paragraphs {
                        for line in para.lines {
                            for word in line.words {
                                words.push((word.text.trim().to_string(), word.confidence));
                            }
                        }
                    }
                }
            }
            Ok(words)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_ocr_returns_scripted() {
        let ocr = MockOcr("hello".into());
        let img = GrayImage::new(1, 1, vec![255]);
        assert_eq!(ocr.recognize(&img, OcrHint::SingleWord).unwrap(), "hello");
    }

    /// End-to-end native OCR: reads a rendered "HELLO" grayscale image and recognizes it.
    /// Requires the `ocr-tesseract` feature (native Tesseract) and `eng.traineddata`. Skips if either
    /// the test image or the tessdata is missing.
    #[cfg(feature = "ocr-tesseract")]
    #[test]
    fn tesseract_recognizes_rendered_text() {
        use std::path::PathBuf;

        fn find_tessdata() -> Option<PathBuf> {
            let mut cands: Vec<PathBuf> = Vec::new();
            if let Ok(p) = std::env::var("TESSDATA_PREFIX") {
                cands.push(PathBuf::from(p));
            }
            if let Ok(appdata) = std::env::var("APPDATA") {
                cands.push(PathBuf::from(&appdata).join("tesseract-rs/aarch64/dynamic/tessdata"));
                cands.push(PathBuf::from(&appdata).join("tesseract-rs/tessdata"));
            }
            cands.push(PathBuf::from("C:/Program Files/Tesseract-OCR/tessdata"));
            cands.into_iter().find(|d| d.join("eng.traineddata").is_file())
        }

        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let (gray, dim) = (dir.join("ocrtest.gray"), dir.join("ocrtest.dim"));
        if !gray.is_file() || !dim.is_file() {
            eprintln!("skip: immagine di test assente");
            return;
        }
        let Some(tessdata) = find_tessdata() else {
            eprintln!("skip: eng.traineddata non trovato");
            return;
        };

        let d = std::fs::read_to_string(&dim).unwrap();
        let mut it = d.split_whitespace();
        let w: u32 = it.next().unwrap().parse().unwrap();
        let h: u32 = it.next().unwrap().parse().unwrap();
        let img = GrayImage::new(w, h, std::fs::read(&gray).unwrap());

        let ocr = TesseractOcr::new(Some(tessdata), "eng", OcrHint::SingleLine).expect("init tesseract");
        let text = ocr.recognize(&img, OcrHint::SingleLine).expect("ocr");
        eprintln!("OCR result: {text:?}");
        assert!(text.to_uppercase().contains("HELLO"), "atteso HELLO, ottenuto {text:?}");
    }
}
