//! Which OCR engine reads a page, and what happens when it cannot.
//!
//! The rule this project inherited from `df-ocr-switcher` — "an ONNX
//! accelerator is present, so use Paddle; plain CPU, so use Tesseract" — is
//! **not** reproduced here, because measuring it refuted it: on this machine's
//! plain CPU, PP-OCRv6 small beats Tesseract on quality *and* speed at every
//! useful level of degradation (`RISULTATI.md` §4-5). Hardware no longer
//! decides the engine.
//!
//! What is left is a preference order with a real fallback: the best engine
//! first, and whatever still opens after it, so a missing ONNX Runtime or an
//! absent model degrades the reading instead of failing the document.

/// An engine name the CLI and the pipeline both understand.
pub type EngineName = &'static str;

/// The order engines are tried in, best first.
///
/// * `v6-small` — the measured default: 97,7 / 95,6 / 91,5 % recall on
///   L0/L1/L2 at 2,6-4,3 s a page.
/// * `tesseract` — the parachute. Slower and weaker above L0, but it is
///   statically linked and needs neither ONNX Runtime nor model files, so it
///   reads when nothing else can.
///
/// `v6-medium` is deliberately absent: it loses letters inside words on
/// tilted or JPEG-compressed pages (L1 recall 71,8 %), and until that is
/// understood it must not be reached by default.
pub const PREFERENCE: [EngineName; 2] = ["v6-small", "tesseract"];

/// The engines to try, in order, honouring an explicit choice.
///
/// A choice made by the caller always wins and is tried alone: asking for an
/// engine and silently getting another is how a measurement becomes a lie.
pub fn candidates(requested: Option<&str>) -> Vec<String> {
    match requested {
        Some(name) => vec![name.to_string()],
        None => PREFERENCE.iter().map(|name| name.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_order_leads_with_the_measured_winner() {
        assert_eq!(candidates(None), ["v6-small", "tesseract"]);
    }

    #[test]
    fn an_explicit_choice_is_tried_alone() {
        // No silent substitution: a run asked for tesseract is a tesseract run
        // or an error, never a v6 run wearing its name.
        assert_eq!(candidates(Some("tesseract")), ["tesseract"]);
        assert_eq!(candidates(Some("v6-medium")), ["v6-medium"]);
    }

    #[test]
    fn the_medium_tier_is_never_reached_by_default() {
        assert!(!PREFERENCE.contains(&"v6-medium"), "see RISULTATI.md §4");
    }
}
