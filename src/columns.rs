//! Where the columns of a page are.
//!
//! Ordering a page by cutting it at empty corridors works only when a corridor
//! is actually empty, and on a real two-column page it never quite is: a rule,
//! a figure, a footnote number crosses it somewhere. The projection histogram
//! is what survives that — count how many lines cover each vertical strip of
//! the page and the gutters show up as troughs, whether or not something
//! crosses them once.
//!
//! The approach, its thresholds and the validation that keeps a list of
//! bullets from reading as a column are taken from
//! `old_project/pdf-inspector/src/extractor/layout.rs` (MIT), re-expressed for
//! the types here. The one deliberate departure: a gutter is a run of strips
//! carrying at most a small *fraction* of the busiest strip, rather than a
//! fixed count, so the same rule holds for a dense journal page and a sparse
//! one.

use crate::geometry::Rect;
use crate::native::Line;

/// Width of one strip of the projection histogram.
const BIN: f32 = 2.0;

/// Narrowest gutter that separates two columns. Below this lies the ordinary
/// white space inside a justified paragraph.
const MIN_GUTTER: f32 = 8.0;

/// Share of the busiest strip a strip may carry and still count as empty.
/// Exactly zero would be defeated by the single footnote marker or rule that
/// crosses a gutter somewhere down the page.
const GUTTER_NOISE: f32 = 0.10;

/// Lines a column needs before it is believed. A margin note, a page number
/// and a list of bullets all sit in their own vertical strip without being
/// columns of text.
const MIN_LINES_PER_COLUMN: usize = 8;

/// Share of the text's own height a column has to span. A column runs down the
/// page; a caption beside a figure does not.
const MIN_VERTICAL_SPAN: f32 = 0.30;

/// A vertical band of the page that text is set in.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Column {
    pub left: f32,
    pub right: f32,
}

impl Column {
    /// Whether a box sits within this column rather than crossing out of it.
    pub fn holds(&self, bbox: &Rect) -> bool {
        let centre = (bbox.left + bbox.right) / 2.0;
        centre >= self.left && centre <= self.right
    }
}

/// The columns a page is set in, left to right. An empty result means the page
/// is one column — or that nothing about it was clear enough to say otherwise.
pub fn detect(lines: &[Line]) -> Vec<Column> {
    let text: Vec<&Line> = lines.iter().filter(|line| line.bbox.width() > 0.0).collect();
    if text.len() < MIN_LINES_PER_COLUMN * 2 {
        return Vec::new();
    }
    let left = text.iter().map(|line| line.bbox.left).fold(f32::MAX, f32::min);
    let right = text.iter().map(|line| line.bbox.right).fold(f32::MIN, f32::max);
    let (top, bottom) = (
        text.iter().map(|line| line.bbox.top).fold(f32::MIN, f32::max),
        text.iter().map(|line| line.bbox.bottom).fold(f32::MAX, f32::min),
    );
    if right - left < MIN_GUTTER || top - bottom <= 0.0 {
        return Vec::new();
    }

    let bins = (((right - left) / BIN).ceil() as usize).max(1);
    let mut histogram = vec![0usize; bins];
    for line in &text {
        let from = (((line.bbox.left - left) / BIN).floor() as usize).min(bins - 1);
        let to = (((line.bbox.right - left) / BIN).ceil() as usize).min(bins);
        for strip in &mut histogram[from..to.max(from + 1)] {
            *strip += 1;
        }
    }

    let busiest = histogram.iter().copied().max().unwrap_or(0) as f32;
    let floor = (busiest * GUTTER_NOISE).floor() as usize;
    let cuts = gutters(&histogram, floor, left);
    let candidates = split_at(cuts, left, right);
    validate(&candidates, &text, top - bottom)
}

/// The middles of the runs of near-empty strips that lie inside the text.
fn gutters(histogram: &[usize], floor: usize, origin: f32) -> Vec<f32> {
    let mut cuts = Vec::new();
    let mut run: Option<usize> = None;
    for (index, &count) in histogram.iter().enumerate() {
        match (count <= floor, run) {
            (true, None) => run = Some(index),
            (false, Some(start)) => {
                push_gutter(&mut cuts, start, index, histogram.len(), origin);
                run = None;
            }
            _ => {}
        }
    }
    // A run reaching the end of the histogram is the right margin, not a gutter.
    cuts
}

fn push_gutter(cuts: &mut Vec<f32>, start: usize, end: usize, bins: usize, origin: f32) {
    // A run touching either edge is a margin: there is no text beyond it.
    if start == 0 || end >= bins {
        return;
    }
    if (end - start) as f32 * BIN >= MIN_GUTTER {
        cuts.push(origin + (start + end) as f32 / 2.0 * BIN);
    }
}

fn split_at(cuts: Vec<f32>, left: f32, right: f32) -> Vec<Column> {
    let mut columns = Vec::with_capacity(cuts.len() + 1);
    let mut start = left;
    for cut in cuts {
        columns.push(Column { left: start, right: cut });
        start = cut;
    }
    columns.push(Column { left: start, right });
    columns
}

/// Keep the split only if every part of it reads as a column: enough lines,
/// and running far enough down the page.
fn validate(candidates: &[Column], lines: &[&Line], height: f32) -> Vec<Column> {
    if candidates.len() < 2 {
        return Vec::new();
    }
    for column in candidates {
        let held: Vec<&&Line> = lines.iter().filter(|line| column.holds(&line.bbox)).collect();
        if held.len() < MIN_LINES_PER_COLUMN {
            return Vec::new();
        }
        let top = held.iter().map(|line| line.bbox.top).fold(f32::MIN, f32::max);
        let bottom = held.iter().map(|line| line.bbox.bottom).fold(f32::MAX, f32::min);
        if height > 0.0 && (top - bottom) / height < MIN_VERTICAL_SPAN {
            return Vec::new();
        }
    }
    candidates.to_vec()
}

/// The column a box belongs to, or `None` when it crosses a gutter — a title
/// or a table that spans the page is read before the columns under it, not
/// inside one of them.
pub fn column_of(columns: &[Column], bbox: &Rect) -> Option<usize> {
    let inside = columns.iter().position(|column| column.holds(bbox))?;
    let crosses = columns
        .iter()
        .enumerate()
        .any(|(index, column)| index != inside && bbox.right > column.left && bbox.left < column.right);
    (!crosses).then_some(inside)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native::{Style, Word};

    fn line(left: f32, bottom: f32, width: f32) -> Line {
        let style = Style { font: "T".into(), size: 10.0, bold: false, monospace: false };
        let bbox = Rect::new(left, bottom, left + width, bottom + 10.0);
        Line {
            words: vec![Word { text: "x".into(), bbox, style, visible: true }],
            bbox,
        }
    }

    /// Two columns of `n` lines each, 200 pt wide, with a 40 pt gutter.
    fn two_columns(n: usize) -> Vec<Line> {
        (0..n)
            .flat_map(|i| {
                let y = 700.0 - i as f32 * 12.0;
                [line(50.0, y, 200.0), line(290.0, y, 200.0)]
            })
            .collect()
    }

    #[test]
    fn a_two_column_page_is_split_at_its_gutter() {
        let columns = detect(&two_columns(30));
        assert_eq!(columns.len(), 2);
        assert!(columns[0].right > 250.0 && columns[0].right < 290.0, "the cut is in the gutter");
    }

    #[test]
    fn a_single_column_page_is_left_alone() {
        let page: Vec<Line> = (0..30).map(|i| line(50.0, 700.0 - i as f32 * 12.0, 440.0)).collect();
        assert!(detect(&page).is_empty());
    }

    #[test]
    fn a_gutter_survives_a_line_that_crosses_it() {
        // A heading spanning both columns must not hide the gutter: that is
        // the whole reason a histogram is used instead of an empty corridor.
        let mut page = two_columns(30);
        page.push(line(50.0, 730.0, 440.0));
        assert_eq!(detect(&page).len(), 2);
    }

    #[test]
    fn a_margin_note_is_not_a_column() {
        // Few lines, and not running down the page: a note beside the text.
        let mut page: Vec<Line> =
            (0..30).map(|i| line(150.0, 700.0 - i as f32 * 12.0, 340.0)).collect();
        page.extend((0..3).map(|i| line(40.0, 600.0 - i as f32 * 12.0, 60.0)));
        assert!(detect(&page).is_empty());
    }

    #[test]
    fn a_line_crossing_the_gutter_belongs_to_no_column() {
        let columns = detect(&two_columns(30));
        let inside = Rect::new(60.0, 500.0, 240.0, 510.0);
        let spanning = Rect::new(50.0, 730.0, 490.0, 745.0);
        assert_eq!(column_of(&columns, &inside), Some(0));
        assert_eq!(column_of(&columns, &spanning), None);
    }

    #[test]
    fn three_columns_come_out_in_order() {
        let page: Vec<Line> = (0..30)
            .flat_map(|i| {
                let y = 700.0 - i as f32 * 12.0;
                [line(40.0, y, 130.0), line(220.0, y, 130.0), line(400.0, y, 130.0)]
            })
            .collect();
        let columns = detect(&page);
        assert_eq!(columns.len(), 3);
        assert!(columns[0].right < columns[1].left + BIN);
        assert!(columns[1].right < columns[2].left + BIN);
    }
}
