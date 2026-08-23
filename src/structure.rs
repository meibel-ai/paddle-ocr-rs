//! Headings without a layout model: from the outline, and from type size.
//!
//! A PDF marks a heading by making it bigger, or bolder, or both — and
//! sometimes by listing it in the outline, which is the only place a document
//! ever *states* its own hierarchy. Both sources are read here, because the
//! geometric path has no model to ask and would otherwise emit a document as
//! one long wall of paragraphs.
//!
//! The rules are deliberately timid. A false heading breaks a document's
//! structure more visibly than a missed one, so a block has to be short and
//! clearly larger than the body before it is promoted.

use std::collections::HashMap;

use crate::assemble::Block;
use crate::native::{Bookmark, Line};

/// Type sizes are compared after rounding to this, so that 9.96 pt and 10.04
/// pt are the same size — which is what a reader sees.
const SIZE_STEP: f32 = 0.5;

/// How much larger than the body a size has to be to mark a heading. Below
/// this lie the incidental differences of a page: a footnote marker, a
/// slightly larger figure caption, an initial.
const HEADING_RATIO: f32 = 1.2;

/// The same for a **bold** line, which needs less size to read as a heading.
const BOLD_RATIO: f32 = 1.05;

/// Lines a block may have and still be a heading. A heading is a label, not a
/// paragraph; without this a whole page of large type becomes one `#`.
const MAX_HEADING_LINES: usize = 3;

/// Words a heading may have. Magazines set pull quotes large and short, and
/// three lines of them still fit the line rule — but no heading runs on for
/// fifteen words, so this is what separates the two.
const MAX_HEADING_WORDS: usize = 15;

/// Heading levels a document can use.
const MAX_LEVEL: u8 = 6;

/// Distinct sizes above the body that are kept as heading tiers — as many as
/// there are heading levels, so that a document setting six display sizes gets
/// six levels instead of collapsing everything past the fourth onto one.
const MAX_TIERS: usize = MAX_LEVEL as usize;

/// The type a document is set in, and what stands out from it.
pub struct Typography {
    body: f32,
    /// Heading sizes, largest first: position in this list is the level.
    tiers: Vec<f32>,
    /// Outline titles, normalised, with the level the document gave them.
    outline: HashMap<String, u8>,
}

impl Typography {
    /// Work out the body size and the heading tiers of a document.
    pub fn of(pages: &[Vec<Line>], bookmarks: &[Bookmark]) -> Self {
        let mut weight: HashMap<u32, usize> = HashMap::new();
        for line in pages.iter().flatten() {
            // Weighted by words, so that one large title cannot outvote the
            // body text it sits above.
            *weight.entry(bucket(line.size())).or_default() += line.words.len().max(1);
        }
        let body = weight
            .iter()
            .max_by_key(|(size, count)| (**count, **size))
            .map(|(size, _)| unbucket(*size))
            .unwrap_or(0.0);

        // The tiers are the heading sizes the document uses *most*, not the
        // largest ones. A magazine sets a dozen display sizes, most of them
        // once; keeping the biggest would leave every recurring section head
        // below the last tier and collapse them all onto one level, while the
        // sizes that come back page after page are exactly its hierarchy.
        let mut larger: Vec<(u32, usize)> = weight
            .iter()
            .filter(|(size, _)| body > 0.0 && unbucket(**size) >= body * HEADING_RATIO)
            .map(|(size, count)| (*size, *count))
            .collect();
        larger.sort_by_key(|(size, count)| (std::cmp::Reverse(*count), *size));
        larger.truncate(MAX_TIERS);
        let mut larger: Vec<f32> = larger.into_iter().map(|(size, _)| unbucket(size)).collect();
        larger.sort_by(|a, b| b.total_cmp(a));

        let outline = bookmarks
            .iter()
            .map(|mark| (normalise(&mark.title), (mark.level as u8 + 1).min(MAX_LEVEL)))
            .collect();
        Typography { body, tiers: larger, outline }
    }

    /// The heading level of a block, or `None` if it reads as body text.
    ///
    /// The outline is asked first: where a document listed a heading, it has
    /// already stated both that it is one and how deep it sits.
    pub fn level_of(&self, block: &Block) -> Option<u8> {
        let text = block.text();
        let normalised = normalise(&text);
        if let Some(level) = self.outline.get(&normalised) {
            return Some(*level);
        }
        // The outline usually lists a section by name while the page prints it
        // with its number ("3 Problem Setup" against "Problem Setup"), so the
        // numbering is dropped before asking again.
        if let Some(level) = self.outline.get(without_numbering(&normalised)) {
            return Some(*level);
        }
        let words = block.lines.iter().map(|line| line.words.len()).sum::<usize>();
        if self.body <= 0.0
            || block.lines.len() > MAX_HEADING_LINES
            || words > MAX_HEADING_WORDS
            || text.trim().is_empty()
        {
            return None;
        }
        let size = block.lines.iter().map(Line::size).fold(0.0, f32::max);
        if size >= self.body * HEADING_RATIO {
            let tier = self.tiers.iter().position(|tier| size >= *tier - SIZE_STEP / 2.0);
            return Some(tier.unwrap_or(self.tiers.len()) as u8 + 1);
        }
        // Bold and a little larger: the way a document sets a run-in heading
        // when it does not want to change size (books do this constantly).
        let bold = block.lines.iter().all(|line| {
            line.words.first().is_some_and(|word| word.style.bold)
        });
        if bold && size >= self.body * BOLD_RATIO {
            return Some((self.tiers.len() as u8 + 1).min(MAX_LEVEL));
        }
        None
    }
}

/// A title without the section number a page prints in front of it: `3.2`,
/// `IV`, `Art. 7`. Only leading tokens made of digits, dots and roman numerals
/// are dropped, so a title that opens with a real word keeps it.
fn without_numbering(title: &str) -> &str {
    let mut rest = title;
    loop {
        let (head, tail) = match rest.split_once(' ') {
            Some(pair) => pair,
            None => return rest,
        };
        let numbering = !head.is_empty()
            && head.chars().all(|c| c.is_ascii_digit() || c == '.' || "ivxlcivxlc".contains(c));
        if !numbering {
            return rest;
        }
        rest = tail;
    }
}

fn bucket(size: f32) -> u32 {
    (size / SIZE_STEP).round() as u32
}

fn unbucket(bucket: u32) -> f32 {
    bucket as f32 * SIZE_STEP
}

/// Text reduced to what makes two titles the same title: an outline entry and
/// the heading on the page differ in spacing, case and trailing punctuation.
fn normalise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;
    use crate::native::{Style, Word};
    use crate::region::RegionKind;

    fn line(text: &str, size: f32, bold: bool) -> Line {
        let style = Style { font: "T".into(), size, bold, monospace: false };
        Line {
            words: text
                .split(' ')
                .map(|word| Word {
                    text: word.into(),
                    bbox: Rect::new(0.0, 0.0, 10.0, size),
                    style: style.clone(),
                    visible: true,
                })
                .collect(),
            bbox: Rect::new(0.0, 0.0, 100.0, size),
        }
    }

    fn block(lines: Vec<Line>) -> Block {
        Block {
            kind: RegionKind::Text,
            bbox: Rect::new(0.0, 0.0, 100.0, 100.0),
            lines,
            order: None,
            recovered: false,
        }
    }

    /// A page of body text at 10 pt with one 18 pt title over it.
    fn a_page() -> Vec<Line> {
        let mut lines = vec![line("Il titolo del capitolo", 18.0, true)];
        for _ in 0..20 {
            lines.push(line("una riga di testo corrente lunga abbastanza da contare", 10.0, false));
        }
        lines
    }

    #[test]
    fn the_body_is_the_size_most_words_are_set_in() {
        let typography = Typography::of(&[a_page()], &[]);
        assert_eq!(typography.body, 10.0);
        assert_eq!(typography.tiers, [18.0]);
    }

    #[test]
    fn a_short_larger_block_is_a_heading_and_a_long_one_is_not() {
        let typography = Typography::of(&[a_page()], &[]);
        assert_eq!(typography.level_of(&block(vec![line("Il titolo", 18.0, true)])), Some(1));

        // Same size, but too many lines to be a label: a pull quote, not a heading.
        let long = block((0..5).map(|_| line("testo grande ma lungo", 18.0, false)).collect());
        assert_eq!(typography.level_of(&long), None);
    }

    #[test]
    fn body_text_stays_body_text() {
        let typography = Typography::of(&[a_page()], &[]);
        let body = block(vec![line("una riga di testo corrente", 10.0, false)]);
        assert_eq!(typography.level_of(&body), None);
    }

    #[test]
    fn a_run_in_heading_is_caught_by_its_weight() {
        // Books set a heading bold at barely more than body size; without the
        // bold rule the whole document would come out flat.
        let typography = Typography::of(&[a_page()], &[]);
        let run_in = block(vec![line("Sezione seconda", 10.5, true)]);
        assert_eq!(typography.level_of(&run_in), Some(2));
        // Not bold at that size: ordinary text.
        assert_eq!(typography.level_of(&block(vec![line("Sezione seconda", 10.5, false)])), None);
    }

    #[test]
    fn the_outline_wins_over_the_type_size() {
        // The document stated its own hierarchy: a sub-sub-heading set in the
        // same type as a chapter title is still a sub-sub-heading.
        let bookmarks = vec![Bookmark {
            title: "Il titolo del capitolo".into(),
            level: 2,
            page: Some(1),
        }];
        let typography = Typography::of(&[a_page()], &bookmarks);
        assert_eq!(typography.level_of(&block(vec![line("Il titolo del capitolo", 18.0, true)])), Some(3));
    }

    #[test]
    fn a_pull_quote_is_not_a_heading() {
        // Magazines set quotes large and short; three lines of them pass the
        // line rule, and 351 of them came out as headings before this.
        let typography = Typography::of(&[a_page()], &[]);
        let quote = block(vec![
            line("questa è una citazione messa in grande che occupa", 18.0, false),
            line("parecchie parole e non è affatto un titolo di sezione", 18.0, false),
        ]);
        assert_eq!(typography.level_of(&quote), None);
    }

    #[test]
    fn a_numbered_heading_still_finds_its_outline_entry() {
        let bookmarks = vec![Bookmark { title: "Problem Setup".into(), level: 1, page: Some(6) }];
        let typography = Typography::of(&[a_page()], &bookmarks);
        assert_eq!(typography.level_of(&block(vec![line("3 Problem Setup", 12.0, true)])), Some(2));
        assert_eq!(typography.level_of(&block(vec![line("3.2.1 Problem Setup", 12.0, true)])), Some(2));
        // A title that opens with a real word keeps it.
        assert_eq!(without_numbering("problem setup"), "problem setup");
    }

    #[test]
    fn a_title_matches_its_outline_entry_across_spacing_and_case() {
        let bookmarks = vec![Bookmark { title: "  1. LA PREMESSA ".into(), level: 0, page: None }];
        let typography = Typography::of(&[a_page()], &bookmarks);
        assert_eq!(typography.level_of(&block(vec![line("1. La premessa", 12.0, false)])), Some(1));
    }
}
