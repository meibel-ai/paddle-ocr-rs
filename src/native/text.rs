//! Turning pdfium's characters into words and lines.
//!
//! pdfium hands back a stream of positioned characters, already decoded
//! through `ToUnicode`, CID maps and embedded cmaps. What it does not hand
//! back is structure: where a line ends, where a word ends, and whether the
//! run of single letters on this line is a word at all. That is this module.
//!
//! Every threshold here is a **fraction of the text's own size**, never a
//! count of points: the same rule then holds for a footnote and a poster
//! (the lesson `old_project/edito-ocr-v6` states in `order.py:105`).

use pdfium_render::prelude::*;

use crate::geometry::Rect;

/// Baseline shift that starts a new line, in ems. The baseline is what "same
/// line" means typographically, and pdfium reports it per character, so the
/// rule needs neither box overlap (which merges tightly-led display type) nor
/// a guess at the leading. Half an em clears a superscript, which sits about a
/// third of an em above its line, and catches even the tightest real leading.
const SAME_LINE_BASELINE_EM: f32 = 0.5;

/// Horizontal gap that ends a line rather than separating two words, in ems.
/// A gap this wide is a column gutter or a table cell boundary — both of which
/// must stay separate units, because columns must never interleave and cells
/// must never merge. Justified prose stays well under it.
const COLUMN_GAP_EM: f32 = 3.0;

/// Gap that separates two words when no space character says so, in ems.
///
/// Two kinds of evidence mark a word boundary, and both are needed. A space
/// character is the producer's own statement and is taken at its word. Where
/// there is none — a producer that positions every glyph itself — only the gap
/// remains. The gap alone cannot carry the rule: pdfium's loose boxes overlap
/// by about half a point on each side, so a real 0.23 em space between two
/// letters measures 0.07 em (seen throughout `AI CNEL.pdf`), and a threshold
/// low enough to catch that would cut ordinary kerning apart.
const WORD_GAP_EM: f32 = 0.18;

/// How much wider than the line's ordinary letter gap a pdfium-invented space
/// has to sit before it is heard as a word break, in ems.
///
/// Half a word gap. Measured on the two lines that pull in opposite
/// directions: on the ROPOLL abstract the invented spaces mark 0.31 em against
/// 0.00 em of ordinary spacing (excess 0.31, heard), on the letterspaced title
/// of `2025_10_24` page 1 they mark 0.105 em against 0.098 em (excess 0.007,
/// ignored). Nothing measured falls between the two.
const HINT_MIN_EXCESS_EM: f32 = WORD_GAP_EM / 2.0;

/// A line has to be at least this many words, mostly single characters, before
/// the letterspacing repair is worth attempting.
const LETTERSPACING_MIN_WORDS: usize = 4;
const LETTERSPACING_SINGLE_SHARE: f32 = 0.6;

/// Bins used to threshold a line's gap distribution.
const OTSU_BINS: usize = 64;

/// What a run of characters looks like. Style drives heading and code
/// detection later, so it travels with the word rather than being re-derived.
#[derive(Debug, Clone, PartialEq)]
pub struct Style {
    pub font: String,
    pub size: f32,
    pub bold: bool,
    pub monospace: bool,
}

/// A word, with the box it occupies on the page.
#[derive(Debug, Clone)]
pub struct Word {
    pub text: String,
    pub bbox: Rect,
    pub style: Style,
    /// `false` for text drawn in an invisible rendering mode: the searchable
    /// layer of a scan, or hidden content. Kept, never dropped.
    pub visible: bool,
}

/// A run of words sharing a baseline.
#[derive(Debug, Clone)]
pub struct Line {
    pub words: Vec<Word>,
    pub bbox: Rect,
}

impl Line {
    fn new(words: Vec<Word>) -> Self {
        let bbox = words.iter().map(|word| word.bbox).collect();
        Line { words, bbox }
    }

    /// The line as text, words separated by single spaces.
    pub fn text(&self) -> String {
        self.words.iter().map(|word| word.text.as_str()).collect::<Vec<_>>().join(" ")
    }

    /// Type size of the line, taken as the largest of its words: a line that
    /// starts with a drop cap is still a body line.
    pub fn size(&self) -> f32 {
        self.words.iter().map(|word| word.style.size).fold(0.0, f32::max)
    }

    /// `true` when no word on the line is painted — a whole line of the
    /// invisible layer.
    pub fn is_invisible(&self) -> bool {
        !self.words.is_empty() && self.words.iter().all(|word| !word.visible)
    }
}

/// One character as pdfium reports it.
struct CharBox {
    value: char,
    bbox: Rect,
    /// `y` of the character's origin: the line it is actually written on.
    baseline: f32,
    style: Style,
    visible: bool,
}

impl CharBox {
    /// A character that draws nothing: whitespace, and the breaks pdfium
    /// generates between text objects. Neither can be part of a word.
    fn is_separator(&self) -> bool {
        self.value.is_whitespace() || self.bbox.width() <= 0.0
    }

    /// A space glyph the producer actually drew, advance and all. This is the
    /// producer's own statement that a word ends here, and it is always taken
    /// at its word.
    fn is_space(&self) -> bool {
        self.value.is_whitespace() && self.bbox.width() > 0.0
    }

    /// A space pdfium inserted itself, which has no advance: its judgement
    /// that these two glyphs sit too far apart for their font. Worth hearing,
    /// but not on its own authority — see `hints_are_word_breaks`.
    ///
    /// A generated *line break* is not one of these. pdfium emits one wherever
    /// a producer starts a new text object, which happens in the middle of a
    /// word often enough (`dell’` + `esecuzione`, 0.6 pt apart on
    /// OJ_L_202402853) that it says nothing about words.
    fn is_hint(&self) -> bool {
        self.is_separator() && !self.is_space() && !matches!(self.value, '\n' | '\r')
    }
}

/// Extract the text of a page as lines of words.
pub fn page_lines(page: &PdfPage) -> Result<Vec<Line>, PdfiumError> {
    let text = page.text()?;
    let chars: Vec<CharBox> = text.chars().iter().filter_map(read_char).collect();
    Ok(group_lines(&chars)
        .into_iter()
        .map(|line| Line::new(build_words(line)))
        .filter(|line| !line.words.is_empty())
        .collect())
}

fn read_char(character: PdfPageTextChar) -> Option<CharBox> {
    let value = character.unicode_char()?;
    // The *loose* box — the character's advance and the font's full height —
    // not the tight ink box. Layout is what matters here: tight boxes make an
    // apostrophe short enough to fall off its own line, and put a digit's side
    // bearings into the gap to the next one, which invents word breaks inside
    // numbers. Tight bounds would only be right for measuring ink.
    let bounds = character.loose_bounds().or_else(|_| character.tight_bounds()).ok()?;
    let visible = !matches!(
        character.render_mode(),
        Ok(PdfPageTextRenderMode::Invisible | PdfPageTextRenderMode::InvisibleClipping)
    );
    let font = character.font_name();
    let bbox = Rect::new(
        bounds.left().value,
        bounds.bottom().value,
        bounds.right().value,
        bounds.top().value,
    );
    Some(CharBox {
        value,
        baseline: character.origin_y().map(|y| y.value).unwrap_or(bbox.bottom),
        bbox,
        style: Style {
            monospace: character.font_is_fixed_pitch() || is_monospace_name(&font),
            bold: is_bold(&character),
            size: character.scaled_font_size().value,
            font,
        },
        visible,
    })
}

fn is_bold(character: &PdfPageTextChar) -> bool {
    let heavy = matches!(
        character.font_weight(),
        Some(
            PdfFontWeight::Weight600
                | PdfFontWeight::Weight700Bold
                | PdfFontWeight::Weight800
                | PdfFontWeight::Weight900
        )
    ) || matches!(character.font_weight(), Some(PdfFontWeight::Custom(weight)) if weight >= 600);
    // Synthetic bold (a font drawn twice, slightly offset) carries no weight.
    heavy || character.font_is_bold_reenforced()
}

/// Font families that mean "this is code", by name — the flag in the font
/// descriptor is missing often enough that the name has to be consulted too
/// (`old_project/pdf-inspector/src/markdown/classify.rs:234`).
fn is_monospace_name(font: &str) -> bool {
    const FAMILIES: [&str; 8] =
        ["courier", "consolas", "monaco", "menlo", "mono", "inconsolata", "hack", "iosevka"];
    let lower = font.to_ascii_lowercase();
    FAMILIES.iter().any(|family| lower.contains(family))
}

/// Split the character stream into lines, on geometry alone.
///
/// Separators are carried along but never consulted: they have no box, so a
/// generated break in the middle of a visual line cannot cut it in two.
fn group_lines(chars: &[CharBox]) -> Vec<&[CharBox]> {
    let mut lines = Vec::new();
    let mut start = 0;
    let mut previous: Option<&CharBox> = None;
    for (index, current) in chars.iter().enumerate() {
        if current.is_separator() {
            continue;
        }
        if previous.is_some_and(|previous| starts_new_line(previous, current)) {
            lines.push(&chars[start..index]);
            start = index;
        }
        previous = Some(current);
    }
    if start < chars.len() {
        lines.push(&chars[start..]);
    }
    lines
}

fn starts_new_line(previous: &CharBox, current: &CharBox) -> bool {
    let em = current.style.size.max(previous.style.size);
    if em <= 0.0 {
        return false;
    }
    if (current.baseline - previous.baseline).abs() > SAME_LINE_BASELINE_EM * em {
        return true;
    }
    // Same baseline, but far to the right: another column, or the next cell of
    // a table. Both have to stay separate units.
    current.bbox.left - previous.bbox.right > COLUMN_GAP_EM * em
}

/// One drawn character of a line, with what precedes it.
struct Glyph<'a> {
    character: &'a CharBox,
    /// A space the producer drew sits between this glyph and the one before.
    after_space: bool,
    /// A space pdfium invented sits there instead.
    after_hint: bool,
    /// Distance from the previous glyph, in ems. Zero for the first.
    gap: f32,
}

/// The drawn characters of a line, each carrying its gap and what kind of
/// space, if any, precedes it.
fn glyphs_of(chars: &[CharBox]) -> Vec<Glyph<'_>> {
    let mut glyphs: Vec<Glyph> = Vec::new();
    let (mut after_space, mut after_hint) = (false, false);
    for character in chars {
        if character.is_separator() {
            after_space |= character.is_space();
            after_hint |= character.is_hint();
            continue;
        }
        let gap = match glyphs.last() {
            Some(previous) => {
                let em = character
                    .style
                    .size
                    .max(previous.character.style.size)
                    .max(f32::EPSILON);
                (character.bbox.left - previous.character.bbox.right) / em
            }
            None => 0.0,
        };
        glyphs.push(Glyph { character, after_space, after_hint, gap });
        (after_space, after_hint) = (false, false);
    }
    glyphs
}

/// Split one line into words: at every space, and wherever the gap exceeds
/// `threshold`. `hear_hints` says whether pdfium's own spaces count too.
fn split_words(glyphs: &[Glyph], threshold: f32, hear_hints: bool) -> Vec<Word> {
    let mut words = Vec::new();
    let mut current: Vec<&CharBox> = Vec::new();
    for glyph in glyphs {
        let boundary =
            glyph.after_space || (hear_hints && glyph.after_hint) || glyph.gap > threshold;
        if !current.is_empty() && boundary {
            push_word(&mut words, &mut current);
        }
        current.push(glyph.character);
    }
    push_word(&mut words, &mut current);
    words
}

/// Build the words of a line, repairing letterspacing when it is there.
fn build_words(chars: &[CharBox]) -> Vec<Word> {
    let glyphs = glyphs_of(chars);
    if glyphs.is_empty() {
        return Vec::new();
    }
    let hear_hints = hints_are_word_breaks(&glyphs);
    let words = split_words(&glyphs, WORD_GAP_EM, hear_hints);
    match letterspaced_threshold(&words, &glyphs) {
        Some(threshold) => split_words(&glyphs, threshold, hear_hints),
        None => words,
    }
}

/// Whether the spaces pdfium invented on this line mark words.
///
/// pdfium inserts a space wherever two glyphs sit further apart than it
/// expects for their font. Two documents show why that judgement can only be
/// heard conditionally, and why the condition is the line's own spacing:
///
/// - On a LaTeX paper (ROPOLL) no space is ever drawn, so the invented ones
///   are the only mark of a word end — and they have to be heard, because a
///   glyph that overhangs its advance closes the gap on its neighbour
///   (`of` + `LLM` measures 0.157 em, under `WORD_GAP_EM`).
/// - On a letterspaced title (`2025_10_24` page 1) every glyph sits 0.10 em
///   from the next and pdfium puts a space in most of those gaps — but not
///   all, and the ones it picks (0.103-0.112 em) are indistinguishable from
///   the ones it skips (0.090-0.100). Heard, they spell `C O MM ISS I O NE`.
///
/// So the hints are heard when the gaps they sit in stand out from the line's
/// ordinary letter gap, and ignored when they are lost in it. Medians, because
/// one wide gap in a line of tight ones must not carry the decision.
fn hints_are_word_breaks(glyphs: &[Glyph]) -> bool {
    // A line whose glyphs report no type size has no em to measure gaps in,
    // so there is nothing to weigh the hints against — pdfium's judgement is
    // then all there is. One page of `ag 434_449318` is like this: without
    // this case two table cells run together as `2024/2853Disposizioni`.
    if glyphs.iter().all(|glyph| glyph.character.style.size <= 0.0) {
        return true;
    }
    let (mut hinted, mut plain) = (Vec::new(), Vec::new());
    for glyph in glyphs.iter().skip(1).filter(|glyph| !glyph.after_space) {
        if glyph.after_hint {
            hinted.push(glyph.gap);
        } else {
            plain.push(glyph.gap);
        }
    }
    match (median(&mut hinted), median(&mut plain)) {
        (Some(wide), Some(ordinary)) => wide - ordinary >= HINT_MIN_EXCESS_EM,
        // Nothing to compare against: a line of nothing but hinted gaps has no
        // ordinary spacing to stand out from, so its hints are all there is.
        (Some(_), None) => true,
        _ => false,
    }
}

fn median(values: &mut [f32]) -> Option<f32> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f32::total_cmp);
    Some(values[values.len() / 2])
}

fn push_word(words: &mut Vec<Word>, chars: &mut Vec<&CharBox>) {
    if chars.is_empty() {
        return;
    }
    let text: String = chars.iter().map(|character| character.value).collect();
    let bbox = chars.iter().map(|character| character.bbox).collect();
    // The first character sets the style: a word rarely changes font mid-way,
    // and when it does the opening run is what the line is about.
    let first = chars[0];
    words.push(Word { text, bbox, style: first.style.clone(), visible: first.visible });
    chars.clear();
}

/// The threshold to re-split a line whose author letterspaced it, or `None`
/// when the line is ordinary.
///
/// Some producers (Canva, and any tool applying tracking) set every letter
/// apart, so a fixed word gap cuts between all of them and the line reads
/// `H e l l o`. The tell-tale is exactly that: a line that came out as mostly
/// single letters. Its gaps then fall into two clear groups — between letters
/// and between words — which is what Otsu's method separates, without a
/// constant that would have to be guessed per document
/// (`old_project/pdf-inspector/src/text_utils.rs:643-890`).
fn letterspaced_threshold(words: &[Word], glyphs: &[Glyph]) -> Option<f32> {
    if words.len() < LETTERSPACING_MIN_WORDS {
        return None;
    }
    let singles = words.iter().filter(|word| word.text.chars().count() == 1).count();
    if (singles as f32 / words.len() as f32) < LETTERSPACING_SINGLE_SHARE {
        return None;
    }
    let gaps: Vec<f32> = glyphs.iter().skip(1).map(|glyph| glyph.gap).collect();
    otsu_threshold(&gaps)
}

/// Otsu's threshold over a set of values: the cut that best separates them
/// into two groups. `None` when the values do not split into two — a line
/// with evenly spaced glyphs is one long word, not many.
fn otsu_threshold(values: &[f32]) -> Option<f32> {
    let (min, max) = values.iter().fold((f32::MAX, f32::MIN), |(lo, hi), &v| (lo.min(v), hi.max(v)));
    if !(max - min).is_finite() || max - min < f32::EPSILON {
        return None;
    }
    let width = (max - min) / OTSU_BINS as f32;
    let mut histogram = [0usize; OTSU_BINS];
    for &value in values {
        let bin = (((value - min) / width) as usize).min(OTSU_BINS - 1);
        histogram[bin] += 1;
    }

    let total: usize = values.len();
    let sum: f32 = histogram.iter().enumerate().map(|(i, &n)| i as f32 * n as f32).sum();
    let (mut below_sum, mut below_count) = (0.0f32, 0usize);
    let (mut best_variance, mut best_bin) = (0.0f32, None);
    for (bin, &count) in histogram.iter().enumerate() {
        below_count += count;
        if below_count == 0 || below_count == total {
            below_sum += bin as f32 * count as f32;
            continue;
        }
        below_sum += bin as f32 * count as f32;
        let above_count = total - below_count;
        let below_mean = below_sum / below_count as f32;
        let above_mean = (sum - below_sum) / above_count as f32;
        let variance =
            below_count as f32 * above_count as f32 * (below_mean - above_mean).powi(2);
        if variance > best_variance {
            best_variance = variance;
            best_bin = Some(bin);
        }
    }
    // A single cluster has no meaningful cut: its between-class variance stays
    // at zero whichever bin is tried.
    best_bin.filter(|_| best_variance > 0.0).map(|bin| min + (bin as f32 + 0.5) * width)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(size: f32) -> Style {
        Style { font: "Test".into(), size, bold: false, monospace: false }
    }

    /// Characters laid out left to right with the given gap before each.
    /// Widths mimic what pdfium reports: a glyph is 0.6 em, a real space
    /// carries its advance, and a generated break has no box at all.
    fn line(spec: &[(char, f32)], size: f32) -> Vec<CharBox> {
        let mut x = 0.0;
        spec.iter()
            .map(|&(value, gap)| {
                let width = match value {
                    '\n' | '\r' => 0.0,
                    ' ' => size * 0.3,
                    _ => size * 0.6,
                };
                x += gap;
                let bbox = Rect::new(x, 0.0, x + width, size);
                x += width;
                CharBox { value, baseline: bbox.bottom, bbox, style: style(size), visible: true }
            })
            .collect()
    }

    #[test]
    fn otsu_finds_the_cut_between_two_clusters() {
        let gaps = [0.05, 0.06, 0.05, 0.9, 0.05, 0.06, 1.0, 0.04];
        let threshold = otsu_threshold(&gaps).expect("two clusters");
        assert!(threshold > 0.06 && threshold < 0.9, "threshold {threshold} splits the clusters");
    }

    #[test]
    fn otsu_refuses_a_single_cluster() {
        assert_eq!(otsu_threshold(&[0.1, 0.1, 0.1, 0.1]), None);
        assert_eq!(otsu_threshold(&[]), None);
    }

    #[test]
    fn letterspaced_text_is_rejoined_into_words() {
        // "Hello world" with 0.3 em of tracking between letters and 0.9 em
        // between words — the Canva case. A fixed word gap cuts between every
        // letter, so without the repair this line reads "H e l l o".
        let spec = [
            ('H', 0.0), ('e', 3.0), ('l', 3.0), ('l', 3.0), ('o', 3.0),
            ('w', 9.0), ('o', 3.0), ('r', 3.0), ('l', 3.0), ('d', 3.0),
        ];
        let chars = line(&spec, 10.0);
        let naive = split_words(&glyphs_of(&chars), WORD_GAP_EM, true);
        assert_eq!(naive.len(), 10, "the fixed gap alone splits every letter");

        let words = build_words(&chars);
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["Hello", "world"]);
    }

    #[test]
    fn ordinary_prose_is_split_on_its_spaces_and_not_re_cut() {
        // Tight kerning inside words, a real space between them: the words come
        // out whole and the letterspacing repair must not fire, or it would
        // re-cut prose on its own guess.
        let spec = [
            ('T', 0.0), ('h', 0.5), ('e', 0.5), (' ', 0.5),
            ('c', 0.5), ('a', 0.5), ('t', 0.5),
        ];
        let words = build_words(&line(&spec, 10.0));
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["The", "cat"]);
    }

    #[test]
    fn a_space_splits_words_even_where_the_boxes_overlap() {
        // Real boxes from AI CNEL.pdf: pdfium's loose boxes overlap by about
        // half a point, which shrinks a genuine 0.23 em space down to 0.07 em.
        // Geometry alone glued every word of that document together, so the
        // space character has to be heard where the producer wrote one.
        let size = 10.7;
        let glyph = |value: char, left: f32, right: f32| CharBox {
            value,
            baseline: 0.0,
            bbox: Rect::new(left, 0.0, right, size),
            style: style(size),
            visible: true,
        };
        let chars = vec![
            glyph('r', 130.7, 134.9),
            glyph('i', 134.4, 137.5),
            glyph(' ', 137.0, 139.5),
            glyph('f', 138.2, 141.8),
            glyph('o', 141.3, 147.1),
        ];
        let words = build_words(&chars);
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["ri", "fo"]);
    }

    #[test]
    fn a_space_pdfium_invented_does_not_split_a_letterspaced_word() {
        // Real boxes from `2025_10_24_Laiuto...` page 1, the title
        // "…NE DI…" of "COMMISSIONE DI STUDIO CNDCEC". Every glyph of that
        // line sits 0.10 em from the next, so pdfium inserts a space of its
        // own between most of them; those measure 0.00 pt, while the space
        // the producer drew measures 2.02. Hearing both cut the title into
        // "NE D I".
        let size = 10.0;
        let glyph = |value: char, left: f32, right: f32| CharBox {
            value,
            baseline: 0.0,
            bbox: Rect::new(left, 0.0, right, size),
            style: style(size),
            visible: true,
        };
        let chars = vec![
            glyph('N', 408.47, 415.67),
            glyph('E', 416.62, 422.32),
            glyph(' ', 423.33, 425.35), // drawn by the producer
            glyph('D', 426.32, 433.36),
            glyph(' ', 434.46, 434.46), // invented by pdfium
            glyph('I', 434.46, 437.39),
        ];
        let words = build_words(&chars);
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["NE", "DI"]);
    }

    #[test]
    fn a_space_pdfium_invented_splits_a_line_that_draws_none() {
        // Real boxes from the ROPOLL abstract: a LaTeX paper draws no space at
        // all, so pdfium's are the only mark of a word end. Geometry cannot
        // stand in for them — `f` overhangs its advance and leaves `of` only
        // 0.157 em from `LLM`, under `WORD_GAP_EM` — but here the invented
        // spaces mark 0.31 em against letters that touch, so they are heard.
        let size = 9.96;
        let glyph = |value: char, left: f32, right: f32| CharBox {
            value,
            baseline: 0.0,
            bbox: Rect::new(left, 0.0, right, size),
            style: style(size),
            visible: true,
        };
        let chars = vec![
            glyph('P', 201.28, 207.49),
            glyph('a', 206.68, 211.77),
            glyph('n', 211.77, 216.85),
            glyph('e', 216.85, 221.36),
            glyph('l', 221.36, 224.19),
            glyph(' ', 227.29, 227.29),
            glyph('o', 227.29, 232.37),
            glyph('f', 230.89, 236.68),
            glyph(' ', 238.31, 238.31),
            glyph('L', 238.24, 243.99),
            glyph('L', 243.89, 249.64),
            glyph('M', 249.44, 258.48),
        ];
        let words = build_words(&chars);
        assert_eq!(
            words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(),
            ["Panel", "of", "LLM"]
        );
    }

    #[test]
    fn a_word_split_across_two_text_objects_stays_one_word() {
        // Producers split a word across text objects and pdfium then reports a
        // generated break inside it (seen on OJ_L_202402853: "dell’" then
        // "esecuzione" 0.6 pt apart). Geometry has to win over that break.
        let spec = [('d', 0.0), ('e', 0.3), ('l', 0.3), ('l', 0.3), ('\n', 0.0), ('o', 0.6)];
        let words = build_words(&line(&spec, 10.0));
        assert_eq!(words.iter().map(|w| w.text.as_str()).collect::<Vec<_>>(), ["dello"]);
    }

    #[test]
    fn a_generated_break_does_not_cut_a_visual_line() {
        let spec = [('a', 0.0), ('\n', 0.0), ('b', 0.5)];
        let chars = line(&spec, 10.0);
        assert_eq!(group_lines(&chars).len(), 1);
    }

    #[test]
    fn a_wide_gap_ends_the_line_so_columns_and_cells_stay_apart() {
        let size = 10.0;
        let left = CharBox { value: 'a', baseline: Rect::new(0.0, 0.0, 6.0, size).bottom, bbox: Rect::new(0.0, 0.0, 6.0, size), style: style(size), visible: true };
        let near = CharBox { value: 'b', baseline: Rect::new(12.0, 0.0, 18.0, size).bottom, bbox: Rect::new(12.0, 0.0, 18.0, size), style: style(size), visible: true };
        let far = CharBox { value: 'c', baseline: Rect::new(300.0, 0.0, 306.0, size).bottom, bbox: Rect::new(300.0, 0.0, 306.0, size), style: style(size), visible: true };
        assert!(!starts_new_line(&left, &near), "a word gap keeps the line");
        assert!(starts_new_line(&left, &far), "a column gutter breaks it");
    }

    #[test]
    fn a_vertical_step_starts_a_new_line() {
        let size = 10.0;
        let first = CharBox { value: 'a', baseline: Rect::new(0.0, 100.0, 6.0, 110.0).bottom, bbox: Rect::new(0.0, 100.0, 6.0, 110.0), style: style(size), visible: true };
        let below = CharBox { value: 'b', baseline: Rect::new(0.0, 85.0, 6.0, 95.0).bottom, bbox: Rect::new(0.0, 85.0, 6.0, 95.0), style: style(size), visible: true };
        assert!(starts_new_line(&first, &below));
    }

    #[test]
    fn a_line_reports_its_text_and_size() {
        let spec = [('a', 0.0), ('b', 0.5), (' ', 0.5), ('c', 0.5)];
        let line = Line::new(build_words(&line(&spec, 12.0)));
        assert_eq!(line.text(), "ab c");
        assert_eq!(line.size(), 12.0);
        assert!(!line.is_invisible());
    }
}
