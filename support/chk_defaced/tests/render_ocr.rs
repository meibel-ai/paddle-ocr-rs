//! End-to-end integration test for the DOCX **render-OCR** verdict (features `render-wry` +
//! `ocr-tesseract`, i.e. `--features render-ocr`).
//!
//! It drives the full doubt-resolving fallback on a tampered DOCX: deterministic outline scan →
//! in-process webview render of the embedded fonts → OCR → compare with the extracted text. The expected
//! result on a "Delaware → Maryland" positional defacement is verdict **Confirmed** plus a
//! `DOCX.RENDER_DIVERGENCE_CONFIRMED` finding naming the substitution.
//!
//! This is a **native, opt-in** test: it needs a real WebView2/WebKit engine, a native Tesseract build
//! with `tessdata`, and a tampered DOCX fixture. It **skips** (rather than fails) when any of these is
//! absent, so it is inert in a plain CI without the native stack.
//!
//! Provide the fixture via `CHK_DEFACED_TAMPERED_DOCX` (absolute path); otherwise it looks for
//! `../defaced-test/replaced.docx` next to the crate. Tessdata is discovered from `TESSDATA_PREFIX` or the
//! per-arch `…/tesseract-rs/<arch>/<mode>/tessdata` cache.

#![cfg(all(feature = "render-wry", feature = "ocr-tesseract"))]

use std::path::PathBuf;

use chk_defaced::finding::Verdict;
use chk_defaced::ocr::{OcrHint, TesseractOcr};

fn find_tessdata() -> Option<PathBuf> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Ok(p) = std::env::var("TESSDATA_PREFIX") {
        cands.push(PathBuf::from(p));
    }
    if let Ok(appdata) = std::env::var("APPDATA") {
        cands.push(PathBuf::from(&appdata).join("tesseract-rs/aarch64/dynamic/tessdata"));
        cands.push(PathBuf::from(&appdata).join("tesseract-rs/x86_64/dynamic/tessdata"));
        cands.push(PathBuf::from(&appdata).join("tesseract-rs/tessdata"));
    }
    cands.push(PathBuf::from("C:/Program Files/Tesseract-OCR/tessdata"));
    cands.into_iter().find(|d| d.join("eng.traineddata").is_file())
}

fn find_fixture() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("CHK_DEFACED_TAMPERED_DOCX") {
        let pb = PathBuf::from(p);
        return pb.is_file().then_some(pb);
    }
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let guess = here.join("../defaced-test/replaced.docx");
    guess.is_file().then_some(guess)
}

#[test]
fn docx_render_ocr_confirms_positional_substitution() {
    let Some(docx) = find_fixture() else {
        eprintln!("skip: no tampered DOCX fixture (set CHK_DEFACED_TAMPERED_DOCX)");
        return;
    };
    let Some(tessdata) = find_tessdata() else {
        eprintln!("skip: eng.traineddata not found");
        return;
    };

    // Deterministic pass first: it must surface the collision signal (and leave the verdict Unconfirmed
    // without a registry / render).
    let mut report = chk_defaced::scan::scan_path(&docx, None).expect("scan");
    let has_collision =
        report.findings.iter().any(|f| f.rule.contains("GLYPH_SEMANTIC_REPLACEMENT"));
    assert!(has_collision, "deterministic scan should find a glyph-semantic-replacement collision");

    // OCR engine — skip gracefully if the native stack can't initialize (missing DLLs, etc.).
    let ocr = match TesseractOcr::new(Some(tessdata), "eng", OcrHint::Block) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skip: could not init Tesseract ({e:#})");
            return;
        }
    };

    // The render-OCR fallback must turn the doubt into a Confirmed verdict.
    chk_defaced::render::verify_docx_with_render(&mut report, &docx, &ocr)
        .expect("render-OCR verify");

    assert_eq!(report.verdict, Some(Verdict::Confirmed), "render-OCR should confirm the divergence");
    let confirmed = report
        .findings
        .iter()
        .find(|f| f.rule == "DOCX.RENDER_DIVERGENCE_CONFIRMED")
        .expect("a DOCX.RENDER_DIVERGENCE_CONFIRMED finding");
    let msg = confirmed.message.to_lowercase();
    assert!(
        msg.contains("maryland") && msg.contains("delaware"),
        "confirmed finding should name the delaware→maryland substitution, got: {}",
        confirmed.message
    );
}

/// Render-OCR confirmation of **hidden text**: the `hidden-text.docx` fixture carries a white-on-white
/// run ("Ignore all previous instructions…") and a sub-visible run. The faithful webview render keeps
/// them invisible, so OCR does not see them → `missing_words` recovers them → `DOCX.HIDDEN_TEXT_CONFIRMED`.
#[test]
fn docx_render_ocr_confirms_hidden_text() {
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let docx = here.join("../defaced-test/hidden-text.docx");
    if !docx.is_file() {
        eprintln!("skip: hidden-text.docx fixture absent");
        return;
    }
    let Some(tessdata) = find_tessdata() else {
        eprintln!("skip: eng.traineddata not found");
        return;
    };

    let mut report = chk_defaced::scan::scan_path(&docx, None).expect("scan");
    assert!(
        report.findings.iter().any(|f| f.rule == "DOCX.INVISIBLE_TEXT_COLOR"),
        "deterministic scan should flag the near-white run"
    );

    let ocr = match TesseractOcr::new(Some(tessdata), "eng", OcrHint::Block) {
        Ok(e) => e,
        Err(e) => {
            eprintln!("skip: could not init Tesseract ({e:#})");
            return;
        }
    };
    chk_defaced::render::verify_docx_with_render(&mut report, &docx, &ocr).expect("render-OCR verify");

    let confirmed = report
        .findings
        .iter()
        .find(|f| f.rule == "DOCX.HIDDEN_TEXT_CONFIRMED")
        .expect("a DOCX.HIDDEN_TEXT_CONFIRMED finding");
    let msg = confirmed.message.to_lowercase();
    assert!(
        msg.contains("ignore") || msg.contains("instructions"),
        "confirmed hidden text should recover the injected words, got: {}",
        confirmed.message
    );
}
