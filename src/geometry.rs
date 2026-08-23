//! The geometry both branches share.
//!
//! Coordinates are PDF user-space points with the origin at the bottom-left
//! corner of the page and `y` growing upwards, which is what pdfium reports
//! and what the OCR branch is mapped back onto.

/// An axis-aligned rectangle.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct Rect {
    pub left: f32,
    pub bottom: f32,
    pub right: f32,
    pub top: f32,
}

impl Rect {
    pub fn new(left: f32, bottom: f32, right: f32, top: f32) -> Self {
        Rect { left, bottom, right, top }
    }

    pub fn width(&self) -> f32 {
        self.right - self.left
    }

    pub fn height(&self) -> f32 {
        self.top - self.bottom
    }

    /// Vertical centre — the closest thing to a baseline that a bounding box
    /// can offer, and what line grouping compares.
    pub fn middle_y(&self) -> f32 {
        (self.bottom + self.top) / 2.0
    }

    /// The smallest rectangle containing both.
    pub fn union(&self, other: &Rect) -> Rect {
        Rect {
            left: self.left.min(other.left),
            bottom: self.bottom.min(other.bottom),
            right: self.right.max(other.right),
            top: self.top.max(other.top),
        }
    }

    /// How much of `self`'s height overlaps `other`'s, as a fraction of the
    /// shorter of the two — the test for "these sit on the same line".
    pub fn vertical_overlap(&self, other: &Rect) -> f32 {
        let shorter = self.height().min(other.height());
        if shorter <= 0.0 {
            return 0.0;
        }
        let overlap = self.top.min(other.top) - self.bottom.max(other.bottom);
        (overlap / shorter).clamp(0.0, 1.0)
    }
}

impl FromIterator<Rect> for Rect {
    /// The bounding box of a sequence. An empty sequence gives an empty rect.
    fn from_iter<I: IntoIterator<Item = Rect>>(rects: I) -> Self {
        rects.into_iter().reduce(|a, b| a.union(&b)).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn union_covers_both() {
        let a = Rect::new(0.0, 0.0, 10.0, 10.0);
        let b = Rect::new(5.0, -5.0, 20.0, 5.0);
        assert_eq!(a.union(&b), Rect::new(0.0, -5.0, 20.0, 10.0));
    }

    #[test]
    fn vertical_overlap_is_relative_to_the_shorter_box() {
        let tall = Rect::new(0.0, 0.0, 1.0, 20.0);
        let short = Rect::new(0.0, 5.0, 1.0, 15.0);
        // The short box is entirely inside the tall one: full overlap.
        assert_eq!(tall.vertical_overlap(&short), 1.0);

        let apart = Rect::new(0.0, 30.0, 1.0, 40.0);
        assert_eq!(tall.vertical_overlap(&apart), 0.0);
    }

    #[test]
    fn a_bounding_box_of_nothing_is_empty() {
        assert_eq!(std::iter::empty().collect::<Rect>(), Rect::default());
    }
}
