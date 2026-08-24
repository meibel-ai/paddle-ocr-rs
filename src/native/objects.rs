//! What a page holds besides text: rules, images and annotations.
//!
//! The rules are the reason this exists. A table in a digital PDF is drawn,
//! not tagged, so its grid has to be read back from the lines the producer
//! painted — which is both cheaper and more exact than asking a model where
//! the cells are, and it is why the native branch detects tables
//! algorithmically instead of running the layout model over them.

use pdfium_render::prelude::*;

use crate::geometry::Rect;

/// Thickness up to which a drawn line counts as a rule rather than a shape.
/// A hairline is 0.5 pt and a heavy table border rarely passes 2 pt; a filled
/// rectangle thicker than this is a panel or a highlight, not a border.
const MAX_RULE_THICKNESS: f32 = 3.0;

/// Length below which a rule is a tick, a bullet or an artefact.
const MIN_RULE_LENGTH: f32 = 4.0;

/// How far a segment may drift off the axis and still count as straight: a
/// rule drawn across a page can miss by a fraction of a point.
const AXIS_TOLERANCE: f32 = 0.6;

/// A straight line drawn on the page: a table border, an underline, a rule.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rule {
    pub bbox: Rect,
    pub horizontal: bool,
}

/// An image as placed on the page, with the index that reaches it again for
/// extraction.
#[derive(Debug, Clone, Copy)]
pub struct ImagePlacement {
    pub bbox: Rect,
    /// Position of the object on its page, for `PdfPage::objects().get()`.
    pub index: usize,
}

/// An annotation carrying something a reader can see or follow.
#[derive(Debug, Clone)]
pub struct Annotation {
    pub kind: PdfPageAnnotationType,
    pub bbox: Rect,
    /// The note's text, for the annotation types that hold one.
    pub contents: Option<String>,
    /// The target of a link.
    pub uri: Option<String>,
}

fn to_rect(bounds: PdfRect) -> Rect {
    Rect::new(bounds.left().value, bounds.bottom().value, bounds.right().value, bounds.top().value)
}

/// The rules drawn on a page.
///
/// Path objects are walked segment by segment rather than taken by their
/// bounding box, because a producer is free to draw a whole table grid as one
/// path — its bounds would then be the table, not its lines.
pub fn rules(page: &PdfPage) -> Vec<Rule> {
    let mut rules = Vec::new();
    for object in page.objects().iter() {
        let Some(path) = object.as_path_object() else { continue };
        let Ok(matrix) = path.matrix() else { continue };
        let mut cursor: Option<(f32, f32)> = None;
        let mut start_of_subpath: Option<(f32, f32)> = None;
        for segment in path.segments().transform(matrix).iter() {
            let point = (segment.x().value, segment.y().value);
            match segment.segment_type() {
                PdfPathSegmentType::MoveTo => {
                    cursor = Some(point);
                    start_of_subpath = Some(point);
                }
                PdfPathSegmentType::LineTo => {
                    if let Some(from) = cursor {
                        rules.extend(rule_between(from, point));
                    }
                    cursor = Some(point);
                }
                // A curve is not a rule; it only moves the cursor.
                PdfPathSegmentType::BezierTo => cursor = Some(point),
                PdfPathSegmentType::Unknown => {}
            }
            if segment.is_close() {
                if let (Some(from), Some(to)) = (cursor, start_of_subpath) {
                    rules.extend(rule_between(from, to));
                }
                cursor = start_of_subpath;
            }
        }
    }
    rules.retain(is_worth_keeping);
    rules
}

/// The rule a segment draws, if it draws one: axis-aligned and long enough.
fn rule_between(from: (f32, f32), to: (f32, f32)) -> Option<Rule> {
    let (dx, dy) = ((to.0 - from.0).abs(), (to.1 - from.1).abs());
    let horizontal = dy <= AXIS_TOLERANCE && dx > dy;
    let vertical = dx <= AXIS_TOLERANCE && dy > dx;
    if !horizontal && !vertical {
        return None;
    }
    Some(Rule {
        bbox: Rect::new(
            from.0.min(to.0),
            from.1.min(to.1),
            from.0.max(to.0),
            from.1.max(to.1),
        ),
        horizontal,
    })
}

fn is_worth_keeping(rule: &Rule) -> bool {
    let (length, thickness) = match rule.horizontal {
        true => (rule.bbox.width(), rule.bbox.height()),
        false => (rule.bbox.height(), rule.bbox.width()),
    };
    length >= MIN_RULE_LENGTH && thickness <= MAX_RULE_THICKNESS
}

/// Where the images sit on a page.
pub fn images(page: &PdfPage) -> Vec<ImagePlacement> {
    page.objects()
        .iter()
        .enumerate()
        .filter(|(_, object)| object.as_image_object().is_some())
        .filter_map(|(index, object)| {
            Some(ImagePlacement { bbox: to_rect(object.bounds().ok()?.to_rect()), index })
        })
        .collect()
}

/// The bitmap of a placed image, decoded and with the page's filters applied,
/// ready to be written next to the Markdown.
///
/// `index` comes from [`images`]. Returns `None` when the object is not an
/// image or pdfium cannot decode it — a figure that fails to come out must not
/// take the page's text with it.
pub fn image_at(
    page: &PdfPage,
    document: &PdfDocument,
    index: usize,
) -> Option<image::DynamicImage> {
    page.objects().get(index).ok()?.as_image_object()?.get_processed_image(document).ok()
}

/// The annotations of a page that carry text or a target. Widgets and the
/// rest are skipped: they add nothing a reader of the Markdown would want.
///
/// A link that points inside the document is dropped along with them: turning
/// those into anchors needs a heading map that only Phase 3 will have.
pub fn annotations(page: &PdfPage) -> Vec<Annotation> {
    page.annotations()
        .iter()
        .filter_map(|annotation| {
            let uri = annotation
                .as_link_annotation()
                .and_then(|link| link.link().ok())
                .and_then(|link| link.action())
                .and_then(|action| action.as_uri_action().and_then(|uri| uri.uri().ok()));
            let contents = annotation.contents().filter(|text| !text.trim().is_empty());
            if uri.is_none() && contents.is_none() {
                return None;
            }
            Some(Annotation {
                kind: annotation.annotation_type(),
                bbox: to_rect(annotation.bounds().ok()?),
                contents,
                uri,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_horizontal_segment_is_a_horizontal_rule() {
        let rule = rule_between((10.0, 100.0), (200.0, 100.0)).expect("a rule");
        assert!(rule.horizontal);
        assert_eq!(rule.bbox, Rect::new(10.0, 100.0, 200.0, 100.0));
    }

    #[test]
    fn a_vertical_segment_is_a_vertical_rule_whichever_way_it_is_drawn() {
        // Producers draw borders bottom-up as often as top-down.
        let up = rule_between((50.0, 20.0), (50.0, 300.0)).expect("a rule");
        let down = rule_between((50.0, 300.0), (50.0, 20.0)).expect("a rule");
        assert!(!up.horizontal && !down.horizontal);
        assert_eq!(up.bbox, down.bbox, "the box does not depend on the direction");
    }

    #[test]
    fn a_slightly_crooked_rule_still_counts() {
        // A rule across a wide page can miss its axis by a fraction of a point.
        assert!(rule_between((10.0, 100.0), (500.0, 100.4)).is_some());
        // A diagonal is a drawing, not a border.
        assert!(rule_between((10.0, 100.0), (200.0, 180.0)).is_none());
    }

    #[test]
    fn ticks_and_thick_bars_are_not_rules() {
        let tick = Rule { bbox: Rect::new(0.0, 0.0, 2.0, 0.0), horizontal: true };
        assert!(!is_worth_keeping(&tick), "too short to be a border");

        let panel = Rule { bbox: Rect::new(0.0, 0.0, 300.0, 40.0), horizontal: true };
        assert!(!is_worth_keeping(&panel), "a filled panel is not a border");

        let border = Rule { bbox: Rect::new(0.0, 0.0, 300.0, 0.5), horizontal: true };
        assert!(is_worth_keeping(&border));
    }
}
