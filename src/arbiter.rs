//! The arbitration policy: Tesseract as the automatic fallback that corrects
//! single words PP-OCRv6 misread.
//!
//! The mechanism is the one the author designed for edito-ocr-v6
//! (`old_project/edito-ocr-v6/src/edito_ocr/arbitrate.py`), re-expressed here:
//! guess the page's language from word-frequency statistics, and for each
//! suspect word — low recogniser confidence, or absent from the lexicon —
//! **re-crop it at higher resolution** from the page raster and read it again
//! with Tesseract in the detected language. The second reading replaces the
//! first only when every safety rule agrees; everything else is left alone,
//! because a wrong correction is worse than a wrong word.
//!
//! The safety rules are v6's, measured there (43 corrections, zero false
//! positives on its corpus):
//! * words only, at least [`MIN_WORD_LEN`] letters — short tokens and codes
//!   change identity too easily;
//! * a token with a digit is **never corrected**, only counted for review:
//!   numbers have no lexicon to arbitrate against;
//! * the replacement must be a word the lexicon knows, replacing one it does
//!   not, similar enough to the original ([`MIN_SIMILARITY`]) and read with
//!   real confidence ([`MIN_TESSERACT_CONFIDENCE`], from v6's specimen work:
//!   genuine letters ≥ 80, confusions ≤ 47).

use image::RgbImage;

use crate::lexicon::Lexicon;

/// Paddle CTC score below which a word is suspect even when the lexicon knows
/// it. Correct words sit above 0.95 on this scale; v6 arbitrated below 98/100.
const SUSPECT_SCORE: f32 = 0.90;

/// Letters a word needs before arbitration may touch it.
const MIN_WORD_LEN: usize = 4;

/// Normalised edit-distance similarity the two readings must share: corrected
/// pairs measured 0.75-0.93 in v6, wrong pairs 0.00-0.40 (`SIM_MIN = 0.6`).
const MIN_SIMILARITY: f32 = 0.6;

/// Tesseract word confidence below which its reading is not trusted.
const MIN_TESSERACT_CONFIDENCE: f32 = 65.0;

/// Magnification applied to the word crop before the second reading (v6 read
/// its crops at ×2).
const CROP_SCALE: u32 = 2;

/// Vertical margin around the word box, as a fraction of its height, so the
/// crop does not shave ascenders and descenders.
const CROP_MARGIN_Y: f32 = 0.25;

/// Horizontal margin, deliberately generous: the letters Paddle *lost* lie
/// outside its word box («fornitur» ends where the unread 'a' begins), so a
/// tight crop hands Tesseract the same amputated word. The fragments of
/// neighbouring words a wide margin pulls in are handled downstream: PSM auto
/// segments them apart and the centre-most reading wins.
const CROP_MARGIN_X: f32 = 0.40;

/// What the arbiter did to one page — carried into the result, because silent
/// corrections are how trust is lost.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Outcome {
    /// `(read by paddle, corrected to)`.
    pub corrections: Vec<(String, String)>,
    /// Suspect tokens containing digits: flagged for review, never touched.
    pub flagged_numbers: usize,
    /// Suspect words where the second reading did not qualify.
    pub declined: usize,
    /// A sample of declined pairs `(paddle, tesseract reading)` for diagnosis;
    /// capped so a garbage page cannot flood the report.
    pub declined_samples: Vec<(String, String)>,
    /// Language the page was arbitrated in.
    pub language: Option<&'static str>,
}

/// One suspect word, as the engine hands it over: its text, its recogniser
/// score, and its box in **raster pixels** (origin top-left).
pub struct Suspect<'a> {
    pub text: &'a str,
    pub score: f32,
    pub left: u32,
    pub top: u32,
    pub right: u32,
    pub bottom: u32,
}

/// Whether arbitration should look at this word at all.
pub fn is_suspect(text: &str, score: f32, language: Option<&'static str>, lexicon: &Lexicon) -> bool {
    let letters = text.chars().filter(|c| c.is_alphabetic()).count();
    if letters < MIN_WORD_LEN || text.chars().count() != letters {
        // Too short, or carries digits/punctuation: handled elsewhere.
        return text.chars().any(|c| c.is_ascii_digit()) && score < SUSPECT_SCORE;
    }
    if score < SUSPECT_SCORE {
        return true;
    }
    match language {
        Some(language) => !lexicon.contains(text, &[language, "eng"]),
        None => false,
    }
}

/// The decision core, engine-free and fully testable: given the two readings,
/// say what the page should carry.
pub fn decide(
    paddle: &str,
    tesseract: Option<(&str, f32)>,
    language: Option<&'static str>,
    lexicon: &Lexicon,
) -> Decision {
    if paddle.chars().any(|c| c.is_ascii_digit()) {
        return Decision::FlagNumber;
    }
    let Some(language) = language else { return Decision::Keep };
    let Some((reading, confidence)) = tesseract else { return Decision::Keep };
    let reading = reading.trim();
    let languages = [language, "eng"];

    let qualified = confidence >= MIN_TESSERACT_CONFIDENCE
        && reading.chars().count() >= MIN_WORD_LEN
        && !reading.chars().any(|c| c.is_ascii_digit())
        && reading != paddle
        && lexicon.contains(reading, &languages)
        && !lexicon.contains(paddle, &languages)
        && similarity(paddle, reading) >= MIN_SIMILARITY;
    if qualified {
        Decision::Replace(reading.to_string())
    } else {
        Decision::Keep
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Decision {
    Keep,
    Replace(String),
    FlagNumber,
}

/// Cut the suspect word out of the page at double size, with margin.
pub fn crop_for_reread(page: &RgbImage, suspect: &Suspect) -> Option<RgbImage> {
    let height = suspect.bottom.saturating_sub(suspect.top);
    if height == 0 || suspect.right <= suspect.left {
        return None;
    }
    let margin_y = (height as f32 * CROP_MARGIN_Y) as u32;
    let margin_x = (height as f32 * CROP_MARGIN_X) as u32;
    let left = suspect.left.saturating_sub(margin_x);
    let top = suspect.top.saturating_sub(margin_y);
    let right = (suspect.right + margin_x).min(page.width());
    let bottom = (suspect.bottom + margin_y).min(page.height());
    if right <= left || bottom <= top {
        return None;
    }
    let crop = image::imageops::crop_imm(page, left, top, right - left, bottom - top).to_image();
    Some(image::imageops::resize(
        &crop,
        crop.width() * CROP_SCALE,
        crop.height() * CROP_SCALE,
        image::imageops::FilterType::CatmullRom,
    ))
}

/// Normalised similarity between two words: `1 - edit_distance / longer_len`.
pub fn similarity(a: &str, b: &str) -> f32 {
    let (a, b): (Vec<char>, Vec<char>) =
        (a.to_lowercase().chars().collect(), b.to_lowercase().chars().collect());
    let longer = a.len().max(b.len());
    if longer == 0 {
        return 1.0;
    }
    let mut previous: Vec<usize> = (0..=b.len()).collect();
    for (i, &ca) in a.iter().enumerate() {
        let mut current = vec![i + 1];
        for (j, &cb) in b.iter().enumerate() {
            let substitution = previous[j] + usize::from(ca != cb);
            current.push(substitution.min(previous[j + 1] + 1).min(current[j] + 1));
        }
        previous = current;
    }
    1.0 - previous[b.len()] as f32 / longer as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn lexicon() -> Lexicon {
        Lexicon::from_test_words(HashMap::from([
            ("ita", vec!["linea", "principio", "fornitura", "della"]),
            ("eng", vec!["line", "supply"]),
        ]))
    }

    #[test]
    fn similarity_matches_the_measured_bands() {
        // v6's measurement: corrected pairs 0.75-0.93, wrong pairs 0.00-0.40.
        assert!(similarity("lnea", "linea") > 0.75);
        assert!(similarity("riura", "fornitura") < 0.6, "too far to correct");
        assert_eq!(similarity("linea", "linea"), 1.0);
    }

    #[test]
    fn a_misread_word_is_replaced_when_every_rule_agrees() {
        let decision = decide("lnea", Some(("linea", 88.0)), Some("ita"), &lexicon());
        assert_eq!(decision, Decision::Replace("linea".into()));
    }

    #[test]
    fn a_number_is_flagged_and_never_corrected() {
        // v6's hardest rule: a warning that is nearly always wrong teaches
        // people to ignore it, and a corrected amount is a lawsuit.
        let decision = decide("10.000", Some(("10.000", 90.0)), Some("ita"), &lexicon());
        assert_eq!(decision, Decision::FlagNumber);
    }

    #[test]
    fn a_reading_the_lexicon_does_not_know_is_declined() {
        let decision = decide("lnea", Some(("lnee", 90.0)), Some("ita"), &lexicon());
        assert_eq!(decision, Decision::Keep);
    }

    #[test]
    fn a_word_the_lexicon_already_knows_is_never_overwritten() {
        // "linea" is a real word: even a confident different reading loses.
        let decision = decide("linea", Some(("lines", 95.0)), Some("ita"), &lexicon());
        assert_eq!(decision, Decision::Keep);
    }

    #[test]
    fn low_tesseract_confidence_declines() {
        let decision = decide("lnea", Some(("linea", 47.0)), Some("ita"), &lexicon());
        assert_eq!(decision, Decision::Keep, "47 is v6's measured confusion band");
    }

    #[test]
    fn suspects_are_low_score_or_out_of_lexicon_words() {
        let lexicon = lexicon();
        assert!(is_suspect("lnea", 0.99, Some("ita"), &lexicon), "not a word: suspect");
        assert!(is_suspect("linea", 0.80, Some("ita"), &lexicon), "low score: suspect");
        assert!(!is_suspect("linea", 0.99, Some("ita"), &lexicon));
        assert!(!is_suspect("al", 0.10, Some("ita"), &lexicon), "too short to arbitrate");
        assert!(is_suspect("10.000", 0.70, Some("ita"), &lexicon), "digits at low score: flag");
    }
}
