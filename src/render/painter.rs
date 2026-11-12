//! The placement reconciler.
//!
//! sixel pixels are painted into the character grid: nothing repaints them,
//! nothing erases them, and any redraw of that region destroys them. Appending
//! sixel bytes to every frame was measured elsewhere at ~283 KB per frame per
//! visible image.
//!
//! So this keeps an immutable snapshot of what is actually on screen, diffs it
//! against what should be, and emits nothing when nothing changed. The caller
//! is responsible for ordering: erases before the text diff, paints after it,
//! so an erase cannot wipe text the diff just wrote.

use super::protocol::ArtProtocol;

/// One image occupying a rectangle of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Placement {
    /// Cache key of the blob to paint.
    pub key: String,
    pub row: u16,
    pub col: u16,
    pub rows: u16,
    pub cols: u16,
}

impl Placement {
    pub fn new(key: impl Into<String>, row: u16, col: u16, rows: u16, cols: u16) -> Self {
        Self {
            key: key.into(),
            row,
            col,
            rows,
            cols,
        }
    }

    pub fn same_rect(&self, other: &Self) -> bool {
        self.row == other.row
            && self.col == other.col
            && self.rows == other.rows
            && self.cols == other.cols
    }

    /// True when the two rectangles share any cell.
    pub fn intersects(&self, other: &Self) -> bool {
        let self_bottom = self.row.saturating_add(self.rows);
        let other_bottom = other.row.saturating_add(other.rows);
        let self_right = self.col.saturating_add(self.cols);
        let other_right = other.col.saturating_add(other.cols);
        self.row < other_bottom
            && other.row < self_bottom
            && self.col < other_right
            && other.col < self_right
    }

    fn erase_op(&self) -> PaintOp {
        PaintOp::Erase {
            row: self.row,
            col: self.col,
            rows: self.rows,
            cols: self.cols,
            key: Some(self.key.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaintOp {
    /// Remove this image. Blanking the cells alone is not enough: sixel pixels
    /// live in the image layer and kitty images are objects above the grid, so
    /// the emitter overpaints (sixel) or deletes by id (kitty). `key` names the
    /// image being removed, which is how the kitty id is derived.
    Erase {
        row: u16,
        col: u16,
        rows: u16,
        cols: u16,
        key: Option<String>,
    },
    /// Write this blob at this rectangle.
    Paint { placement: Placement },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FramePlan {
    pub erases: Vec<PaintOp>,
    pub paints: Vec<PaintOp>,
}

impl FramePlan {
    pub fn is_empty(&self) -> bool {
        self.erases.is_empty() && self.paints.is_empty()
    }

    pub fn op_count(&self) -> usize {
        self.erases.len() + self.paints.len()
    }
}

pub struct Painter {
    protocol: ArtProtocol,
    painted: Vec<Placement>,
}

impl Painter {
    pub fn new(protocol: ArtProtocol) -> Self {
        Self {
            protocol,
            painted: Vec::new(),
        }
    }

    pub fn painted(&self) -> &[Placement] {
        &self.painted
    }

    /// Forget what is on screen. Call on resize, on entering the alternate
    /// screen, and after any external write to the terminal.
    pub fn invalidate(&mut self) {
        self.painted.clear();
    }

    /// Diff the desired placements against what is on screen.
    ///
    /// The common case — music playing, nothing moving — returns an empty plan.
    pub fn plan(&mut self, desired: &[Placement]) -> FramePlan {
        // Only pixel protocols place images; symbols are drawn as cells by
        // ratatui and `off` draws nothing.
        if !self.protocol.is_pixel() {
            self.painted.clear();
            return FramePlan::default();
        }

        let mut plan = FramePlan::default();

        // Erase anything gone, moved, resized, or overlapped by a different
        // image: a partial overpaint would leave sixel artifacts behind.
        for old in &self.painted {
            let exact_match = desired
                .iter()
                .any(|new| new.key == old.key && new.same_rect(old));
            let hidden = desired
                .iter()
                .any(|new| new.key != old.key && new.intersects(old));
            if !exact_match || hidden {
                plan.erases.push(old.erase_op());
            }
        }

        // Paint anything new or changed.
        for new in desired {
            let unchanged = self
                .painted
                .iter()
                .any(|old| old.key == new.key && old.same_rect(new));
            if !unchanged {
                plan.paints.push(PaintOp::Paint {
                    placement: new.clone(),
                });
            }
        }

        self.painted = desired.to_vec();
        plan
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn place(key: &str, row: u16, col: u16) -> Placement {
        Placement::new(key, row, col, 2, 4)
    }

    #[test]
    fn first_frame_paints_everything() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        let plan = painter.plan(&[place("a", 0, 0), place("b", 2, 0)]);
        assert_eq!(plan.erases.len(), 0);
        assert_eq!(plan.paints.len(), 2);
        assert_eq!(painter.painted().len(), 2);
    }

    #[test]
    fn an_unchanged_frame_emits_nothing() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        let placements = [place("a", 0, 0), place("b", 2, 0)];
        painter.plan(&placements);
        let plan = painter.plan(&placements);
        assert!(plan.is_empty(), "idle frame must cost zero bytes");
        assert_eq!(plan.op_count(), 0);
    }

    #[test]
    fn scrolling_one_row_erases_and_repaints_each_image() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 2, 0), place("b", 4, 0)]);
        // The list scrolls up by two rows: both images move.
        let plan = painter.plan(&[place("a", 0, 0), place("b", 2, 0)]);
        assert_eq!(plan.erases.len(), 2);
        assert_eq!(plan.paints.len(), 2);
    }

    #[test]
    fn removing_an_image_erases_only_that_one() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 0, 0), place("b", 2, 0)]);
        let plan = painter.plan(&[place("a", 0, 0)]);
        assert_eq!(plan.erases.len(), 1);
        assert!(plan.paints.is_empty());
    }

    #[test]
    fn clearing_the_screen_erases_everything() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 0, 0), place("b", 2, 0)]);
        let plan = painter.plan(&[]);
        assert_eq!(plan.erases.len(), 2);
        assert!(plan.paints.is_empty());
        assert!(painter.painted().is_empty());
    }

    #[test]
    fn a_different_image_in_the_same_rect_repaints() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 0, 0)]);
        let plan = painter.plan(&[place("b", 0, 0)]);
        assert_eq!(plan.erases.len(), 1, "old image must be cleared first");
        assert_eq!(plan.paints.len(), 1);
    }

    #[test]
    fn growing_a_placement_erases_the_old_rect_first() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[Placement::new("a", 0, 0, 2, 4)]);
        let plan = painter.plan(&[Placement::new("a", 0, 0, 3, 6)]);
        assert_eq!(plan.erases.len(), 1);
        assert_eq!(plan.paints.len(), 1);
    }

    #[test]
    fn invalidate_forces_a_full_repaint() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        let placements = [place("a", 0, 0)];
        painter.plan(&placements);
        painter.invalidate();
        let plan = painter.plan(&placements);
        assert_eq!(plan.paints.len(), 1);
        assert!(plan.erases.is_empty(), "nothing is known to be on screen");
    }

    #[test]
    fn symbols_mode_never_places_pixels() {
        let mut painter = Painter::new(ArtProtocol::Symbols);
        let plan = painter.plan(&[place("a", 0, 0)]);
        assert!(plan.is_empty());
        assert!(painter.painted().is_empty());
    }

    #[test]
    fn off_mode_never_places_pixels() {
        let mut painter = Painter::new(ArtProtocol::Off);
        assert!(painter.plan(&[place("a", 0, 0)]).is_empty());
    }

    #[test]
    fn overlap_detection_is_geometric() {
        let a = Placement::new("a", 0, 0, 2, 4);
        let b = Placement::new("b", 1, 2, 2, 4);
        let c = Placement::new("c", 5, 5, 2, 4);
        assert!(a.intersects(&b));
        assert!(b.intersects(&a));
        assert!(!a.intersects(&c));
        // Adjacent but not overlapping.
        let edge = Placement::new("d", 2, 0, 2, 4);
        assert!(!a.intersects(&edge));
    }

    #[test]
    fn a_covering_image_erases_the_one_underneath() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[Placement::new("old", 0, 0, 2, 4)]);
        let plan = painter.plan(&[Placement::new("new", 0, 0, 4, 8)]);
        assert_eq!(plan.erases.len(), 1);
        assert_eq!(plan.paints.len(), 1);
    }
}
