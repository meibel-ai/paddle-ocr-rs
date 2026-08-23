//! Putting a page back together: which lines belong to which region, and in
//! what order they are read.
//!
//! Two rules govern this, both learned the hard way by the pipelines this one
//! draws on:
//!
//! * **the layout gives structure, it does not decide what exists**
//!   (`old_project/edito-ocr-v6/src/edito_ocr/order.py:3-7`). A line that
//!   falls outside every region is not dropped; it is clustered with its
//!   neighbours into a region of its own and read in its place;
//! * **ordering reorders, it does not filter**
//!   (`.../pipeline.py:487-491`). Every line that goes in comes out, and
//!   [`assemble`] states that as an assertion rather than a hope.

use crate::geometry::Rect;
use crate::native::Line;
use crate::region::{Region, RegionKind};

/// Share of a line's box that must fall inside a region to belong to it.
/// Generous on purpose: a region box is drawn around the text it holds, so a
/// descender or an italic overhang routinely pokes out of it.
const INSIDE_SHARE: f32 = 0.5;

/// Vertical gap that separates two clusters of orphan lines, as a multiple of
/// their own median height — never a count of points, so the rule holds at any
/// type size (`old_project/edito-ocr-v6/src/edito_ocr/order.py:105`).
const ORPHAN_GAP_Y: f32 = 1.8;
/// The same horizontally: orphans further apart than this are separate things.
const ORPHAN_GAP_X: f32 = 3.0;

/// Minimum width of the empty corridor that an XY cut may split on.
const MIN_CORRIDOR: f32 = 8.0;

/// Recursion depth for the geometric fallback ordering.
const MAX_CUT_DEPTH: usize = 16;

/// A region together with the lines that fall in it.
#[derive(Debug, Clone)]
pub struct Block {
    pub kind: RegionKind,
    pub bbox: Rect,
    pub lines: Vec<Line>,
    /// The reading position the region stated, carried here because the
    /// blocks are not in step with the regions: a region that claimed no line
    /// produces no block.
    pub order: Option<u32>,
    /// `true` when no region claimed these lines and geometry grouped them.
    pub recovered: bool,
}

impl Block {
    /// The block's text, one line per line.
    pub fn text(&self) -> String {
        self.lines.iter().map(Line::text).collect::<Vec<_>>().join("\n")
    }
}

/// Assign lines to regions and put the result in reading order.
pub fn assemble(lines: Vec<Line>, regions: &[Region]) -> Vec<Block> {
    let count_in = lines.len();
    let (mut blocks, orphans) = distribute(lines, regions);
    blocks.extend(recover(orphans));

    let ordered = order_blocks(blocks);
    debug_assert_eq!(
        ordered.iter().map(|block| block.lines.len()).sum::<usize>(),
        count_in,
        "ordering reorders, it does not filter",
    );
    ordered
}

/// Hand each line to the region that best contains it.
fn distribute(lines: Vec<Line>, regions: &[Region]) -> (Vec<Block>, Vec<Line>) {
    let mut held: Vec<Vec<Line>> = vec![Vec::new(); regions.len()];
    let mut orphans = Vec::new();
    for line in lines {
        match best_region(&line.bbox, regions) {
            Some(index) => held[index].push(line),
            None => orphans.push(line),
        }
    }
    let blocks = regions
        .iter()
        .zip(held)
        .filter(|(_, lines)| !lines.is_empty())
        .map(|(region, lines)| Block {
            kind: region.kind,
            bbox: region.bbox,
            lines,
            order: region.order,
            recovered: false,
        })
        .collect();
    (blocks, orphans)
}

/// The region a box belongs to: the one that covers most of it.
fn best_region(bbox: &Rect, regions: &[Region]) -> Option<usize> {
    regions
        .iter()
        .enumerate()
        .map(|(index, region)| (index, covered_share(bbox, &region.bbox)))
        .filter(|(_, share)| *share >= INSIDE_SHARE)
        .max_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(index, _)| index)
}

/// How much of `inner` lies inside `outer`, by area.
fn covered_share(inner: &Rect, outer: &Rect) -> f32 {
    let width = (inner.right.min(outer.right) - inner.left.max(outer.left)).max(0.0);
    let height = (inner.top.min(outer.top) - inner.bottom.max(outer.bottom)).max(0.0);
    let area = inner.width() * inner.height();
    if area <= 0.0 {
        return 0.0;
    }
    (width * height / area).clamp(0.0, 1.0)
}

/// Turn the lines no region claimed into blocks of their own, by clustering
/// the ones that sit together.
fn recover(mut orphans: Vec<Line>) -> Vec<Block> {
    if orphans.is_empty() {
        return Vec::new();
    }
    let median_height = median(orphans.iter().map(|line| line.bbox.height()));
    let (gap_y, gap_x) = (ORPHAN_GAP_Y * median_height, ORPHAN_GAP_X * median_height);

    // Down the page, then across: the order lines are met in.
    orphans.sort_by(|a, b| {
        b.bbox.top.total_cmp(&a.bbox.top).then(a.bbox.left.total_cmp(&b.bbox.left))
    });

    let mut blocks: Vec<Block> = Vec::new();
    for line in orphans {
        let joins = blocks.last().is_some_and(|block| {
            let vertical = block.bbox.bottom - line.bbox.top;
            let horizontal =
                (line.bbox.left - block.bbox.right).max(block.bbox.left - line.bbox.right);
            vertical <= gap_y && horizontal <= gap_x
        });
        match (joins, blocks.last_mut()) {
            (true, Some(block)) => {
                block.bbox = block.bbox.union(&line.bbox);
                block.lines.push(line);
            }
            _ => blocks.push(Block {
                kind: RegionKind::Text,
                bbox: line.bbox,
                lines: vec![line],
                order: None,
                recovered: true,
            }),
        }
    }
    blocks
}

fn median(values: impl Iterator<Item = f32>) -> f32 {
    let mut values: Vec<f32> = values.collect();
    if values.is_empty() {
        return 0.0;
    }
    values.sort_by(f32::total_cmp);
    values[values.len() / 2]
}

/// Put the blocks in reading order.
///
/// Where a source stated an order, it is followed. A block it did not place —
/// a recovered orphan, or a region it forgot to number — is not appended at
/// the end but slotted in beside the block it sits nearest, so it is read
/// where it appears (`old_project/edito-ocr-v6/src/edito_ocr/order.py:95`).
/// With nothing stated at all, geometry decides.
fn order_blocks(mut blocks: Vec<Block>) -> Vec<Block> {
    if blocks.iter().all(|block| block.order.is_none()) {
        let boxes: Vec<Rect> = blocks.iter().map(|block| block.bbox).collect();
        let mut taken: Vec<Option<Block>> = blocks.drain(..).map(Some).collect();
        return xy_cut(&boxes).into_iter().filter_map(|index| taken[index].take()).collect();
    }
    let keys = sort_keys(&blocks);
    let mut keyed: Vec<(f32, Block)> = keys.into_iter().zip(blocks).collect();
    keyed.sort_by(|a, b| a.0.total_cmp(&b.0));
    keyed.into_iter().map(|(_, block)| block).collect()
}

/// A sort key per block: its stated order, or just past the nearest block
/// that has one.
fn sort_keys(blocks: &[Block]) -> Vec<f32> {
    blocks
        .iter()
        .map(|block| match block.order {
            Some(order) => order as f32,
            None => nearest_stated(block, blocks).map_or(f32::MAX, |order| order as f32 + 0.5),
        })
        .collect()
}

/// The stated order of the placed block whose box lies nearest to `block`.
fn nearest_stated(block: &Block, blocks: &[Block]) -> Option<u32> {
    blocks
        .iter()
        .filter_map(|other| Some((other.order?, distance(&block.bbox, &other.bbox))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .map(|(order, _)| order)
}

/// Distance between the centres of two boxes.
fn distance(a: &Rect, b: &Rect) -> f32 {
    let dx = (a.left + a.right) / 2.0 - (b.left + b.right) / 2.0;
    let dy = a.middle_y() - b.middle_y();
    (dx * dx + dy * dy).sqrt()
}

/// Reading order by recursive XY cut.
///
/// The cut is taken at the **widest** empty corridor, not the first one found:
/// splitting on the first corridor slices a page into bands that mix its
/// columns together (`old_project/edito-ocr-v6/src/edito_ocr/order.py:58-70`).
pub fn xy_cut(boxes: &[Rect]) -> Vec<usize> {
    let mut order = Vec::with_capacity(boxes.len());
    cut(boxes, &(0..boxes.len()).collect::<Vec<_>>(), 0, &mut order);
    order
}

fn cut(boxes: &[Rect], group: &[usize], depth: usize, order: &mut Vec<usize>) {
    if group.len() <= 1 || depth >= MAX_CUT_DEPTH {
        let mut sorted = group.to_vec();
        sorted.sort_by(|&a, &b| {
            boxes[b].top.total_cmp(&boxes[a].top).then(boxes[a].left.total_cmp(&boxes[b].left))
        });
        order.extend(sorted);
        return;
    }
    // A vertical corridor separates columns and is read left to right; a
    // horizontal one separates bands and is read top to bottom. Columns win
    // when both exist, or a two-column page reads across its columns.
    if let Some(at) = widest_corridor(group, |index| (boxes[index].left, boxes[index].right)) {
        let (left, right): (Vec<usize>, Vec<usize>) =
            group.iter().partition(|&&index| boxes[index].right <= at);
        if !left.is_empty() && !right.is_empty() {
            cut(boxes, &left, depth + 1, order);
            cut(boxes, &right, depth + 1, order);
            return;
        }
    }
    if let Some(at) = widest_corridor(group, |index| (-boxes[index].top, -boxes[index].bottom)) {
        let (above, below): (Vec<usize>, Vec<usize>) =
            group.iter().partition(|&&index| -boxes[index].bottom <= at);
        if !above.is_empty() && !below.is_empty() {
            cut(boxes, &above, depth + 1, order);
            cut(boxes, &below, depth + 1, order);
            return;
        }
    }
    cut(boxes, group, MAX_CUT_DEPTH, order);
}

/// The end of the widest empty corridor along one axis, if one is wide enough
/// to be a real gutter. `extent` yields each box's `(start, end)` on that axis.
fn widest_corridor(group: &[usize], extent: impl Fn(usize) -> (f32, f32)) -> Option<f32> {
    let mut spans: Vec<(f32, f32)> = group.iter().map(|&index| extent(index)).collect();
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));

    let (mut reach, mut best) = (spans[0].1, None::<(f32, f32)>);
    for &(start, end) in &spans[1..] {
        let gap = start - reach;
        if gap >= MIN_CORRIDOR && best.is_none_or(|(width, _)| gap > width) {
            best = Some((gap, reach));
        }
        reach = reach.max(end);
    }
    best.map(|(_, at)| at)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn region(left: f32, bottom: f32, right: f32, top: f32, order: Option<u32>) -> Region {
        Region {
            bbox: Rect::new(left, bottom, right, top),
            kind: RegionKind::Text,
            score: 0.9,
            order,
        }
    }

    #[test]
    fn a_box_belongs_to_the_region_that_covers_most_of_it() {
        let regions =
            [region(0.0, 0.0, 100.0, 100.0, None), region(90.0, 0.0, 200.0, 100.0, None)];
        // Mostly in the first, poking into the second: still the first.
        assert_eq!(best_region(&Rect::new(10.0, 10.0, 95.0, 20.0), &regions), Some(0));
        // Well outside both.
        assert_eq!(best_region(&Rect::new(300.0, 10.0, 400.0, 20.0), &regions), None);
    }

    #[test]
    fn a_column_gutter_is_cut_before_a_band() {
        // Two columns of two blocks each. Read down the left, then the right —
        // not across, which is what a horizontal cut would produce.
        let boxes = [
            Rect::new(0.0, 500.0, 200.0, 600.0),   // 0 left top
            Rect::new(0.0, 300.0, 200.0, 400.0),   // 1 left bottom
            Rect::new(300.0, 500.0, 500.0, 600.0), // 2 right top
            Rect::new(300.0, 300.0, 500.0, 400.0), // 3 right bottom
        ];
        assert_eq!(xy_cut(&boxes), [0, 1, 2, 3]);
    }

    #[test]
    fn a_full_width_banner_is_read_before_the_columns_below_it() {
        let boxes = [
            Rect::new(0.0, 300.0, 200.0, 400.0),   // 0 left column
            Rect::new(300.0, 300.0, 500.0, 400.0), // 1 right column
            Rect::new(0.0, 500.0, 500.0, 600.0),   // 2 the banner
        ];
        assert_eq!(xy_cut(&boxes), [2, 0, 1]);
    }

    #[test]
    fn the_widest_corridor_wins_over_the_first_one() {
        // A narrow gap at 210 and a real gutter at 300: cutting at the first
        // would put block 1 with the left column instead of the right.
        let boxes = [
            Rect::new(0.0, 0.0, 200.0, 100.0),
            Rect::new(212.0, 0.0, 260.0, 100.0),
            Rect::new(360.0, 0.0, 560.0, 100.0),
        ];
        let corridor = widest_corridor(&[0, 1, 2], |index| (boxes[index].left, boxes[index].right));
        assert_eq!(corridor, Some(260.0), "the 100 pt gutter, not the 12 pt gap");
    }

    #[test]
    fn nothing_is_lost_and_orphans_come_back() {
        let region_lines = Rect::new(0.0, 500.0, 200.0, 600.0);
        let regions = [region(region_lines.left, 500.0, 200.0, 600.0, None)];
        let lines = vec![
            line_at(Rect::new(10.0, 520.0, 190.0, 535.0)), // inside the region
            line_at(Rect::new(10.0, 100.0, 190.0, 115.0)), // orphan
            line_at(Rect::new(10.0, 80.0, 190.0, 95.0)),   // orphan, same cluster
        ];
        let blocks = assemble(lines, &regions);
        assert_eq!(blocks.iter().map(|block| block.lines.len()).sum::<usize>(), 3);
        let recovered: Vec<&Block> = blocks.iter().filter(|block| block.recovered).collect();
        assert_eq!(recovered.len(), 1, "the two orphans cluster into one block");
        assert_eq!(recovered[0].lines.len(), 2);
    }

    #[test]
    fn a_stated_order_is_followed_when_every_region_has_one() {
        let regions = [
            region(0.0, 0.0, 200.0, 100.0, Some(7)),
            region(0.0, 500.0, 200.0, 600.0, Some(3)),
        ];
        let lines = vec![
            line_at(Rect::new(10.0, 10.0, 190.0, 25.0)),
            line_at(Rect::new(10.0, 520.0, 190.0, 535.0)),
        ];
        let blocks = assemble(lines, &regions);
        // Region 1 states 3 and region 0 states 7, so the lower box is read
        // second even though it sits higher on the page.
        assert_eq!(blocks[0].bbox.bottom, 500.0);
        assert_eq!(blocks[1].bbox.bottom, 0.0);
    }

    /// A line with no words, positioned — enough for assignment and ordering.
    fn line_at(bbox: Rect) -> Line {
        Line { words: Vec::new(), bbox }
    }

    #[test]
    fn a_region_that_claimed_no_line_does_not_shift_the_others() {
        // Blocks are not in step with regions: an empty region produces no
        // block, so an order looked up by position lands on the wrong block.
        let regions = [
            region(0.0, 700.0, 200.0, 800.0, Some(1)), // claims nothing
            region(0.0, 500.0, 200.0, 600.0, Some(9)), // read last
            region(0.0, 300.0, 200.0, 400.0, Some(2)), // read first
        ];
        let lines = vec![
            line_at(Rect::new(10.0, 520.0, 190.0, 535.0)),
            line_at(Rect::new(10.0, 320.0, 190.0, 335.0)),
        ];
        let blocks = assemble(lines, &regions);
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0].order, Some(2), "the region stating 2 comes first");
        assert_eq!(blocks[1].order, Some(9));
    }
}
