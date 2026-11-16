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
//!
//! That snapshot is not complete on its own. An image sits in the same grid as
//! the characters, so the text diff writes over one every time a cell under it
//! changes - a selection bar moving onto a row is enough - and no placement
//! moved, so the diff above sees nothing. `repaint_damaged` closes that hole:
//! it compares the cells under each image with the ones it left there, and asks
//! for the images the frame wrote over.

use ratatui::buffer::{Buffer, Cell};

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
    /// The cells each painted image covers, as the last frame left them, in
    /// the same order as `painted`. `None` means the image went up this frame
    /// and no frame has been written over it yet.
    under: Vec<Option<Vec<Cell>>>,
}

impl Painter {
    pub fn new(protocol: ArtProtocol) -> Self {
        Self {
            protocol,
            painted: Vec::new(),
            under: Vec::new(),
        }
    }

    pub fn painted(&self) -> &[Placement] {
        &self.painted
    }

    /// Forget what is on screen. Call on resize, on entering the alternate
    /// screen, and after any external write to the terminal.
    pub fn invalidate(&mut self) {
        self.painted.clear();
        self.under.clear();
    }

    /// Diff the desired placements against what is on screen.
    ///
    /// The common case - music playing, nothing moving - returns an empty plan.
    pub fn plan(&mut self, desired: &[Placement]) -> FramePlan {
        // Only pixel protocols place images; symbols are drawn as cells by
        // ratatui.
        if !self.protocol.is_pixel() {
            self.painted.clear();
            self.under.clear();
            return FramePlan::default();
        }

        let mut plan = FramePlan::default();

        // The common frame: every image is where it was, showing what it was.
        // Nothing to erase, nothing to paint, and the cells under them are
        // still the ones recorded. Placements never overlap - the layouts are
        // tested for it - so an image cannot be hiding another one here.
        if self.painted.len() == desired.len()
            && self
                .painted
                .iter()
                .zip(desired)
                .all(|(old, new)| old.key == new.key && old.same_rect(new))
        {
            return plan;
        }

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

        // Paint anything new or changed, and carry the cell snapshot of the
        // images that stay put: those are the only ones the frame in between
        // has not been drawn over yet. Moved, not cloned - a still frame
        // allocates nothing for the cells.
        let mut carried = std::mem::take(&mut self.under);
        let mut under = Vec::with_capacity(desired.len());
        for new in desired {
            let unchanged = self
                .painted
                .iter()
                .position(|old| old.key == new.key && old.same_rect(new));
            match unchanged {
                Some(index) => under.push(carried.get_mut(index).and_then(Option::take)),
                None => {
                    plan.paints.push(PaintOp::Paint {
                        placement: new.clone(),
                    });
                    under.push(None);
                }
            }
        }

        self.under = under;
        self.painted = desired.to_vec();
        plan
    }

    /// Repaint any image the text frame just wrote over.
    ///
    /// Call this after the frame is drawn, with the buffer that was drawn, and
    /// paint what comes back. An image that went up this frame is left alone:
    /// it was painted after the text, so it is the top layer.
    ///
    /// kitty images are objects above the grid, so text written under one is
    /// hidden by it rather than destructive, and those frames stay free.
    pub fn repaint_damaged(&mut self, buffer: &Buffer) -> FramePlan {
        // Only sixel needs this: a kitty image is an object above the grid, so
        // text written under one is hidden by it rather than destructive.
        if self.protocol != ArtProtocol::Sixel {
            return FramePlan::default();
        }

        let mut plan = FramePlan::default();
        for (placement, under) in self.painted.iter().zip(self.under.iter_mut()) {
            let damaged = under
                .as_deref()
                .is_some_and(|before| written_over(buffer, placement, before));
            if damaged {
                plan.paints.push(PaintOp::Paint {
                    placement: placement.clone(),
                });
            }
            // Only a damaged image needs a fresh snapshot: the others are
            // still showing the cells they recorded.
            if under.is_none() || damaged {
                let mut cells = under.take().unwrap_or_default();
                snapshot_under(buffer, placement, &mut cells);
                *under = Some(cells);
            }
        }
        plan
    }
}

/// True when the frame wrote on any cell the image covers.
fn written_over(buffer: &Buffer, placement: &Placement, before: &[Cell]) -> bool {
    let area = buffer.area;
    // What a cell off the screen compares against, now and before.
    let outside = Cell::default();
    let mut index = 0;
    for row in placement.row..placement.row.saturating_add(placement.rows) {
        for col in placement.col..placement.col.saturating_add(placement.cols) {
            let on_screen =
                row >= area.y && row < area.bottom() && col >= area.x && col < area.right();
            let now = if on_screen {
                &buffer[(col, row)]
            } else {
                &outside
            };
            if before.get(index) != Some(now) {
                return true;
            }
            index += 1;
        }
    }
    false
}

/// Record the cells an image covers, as the frame just left them.
///
/// Fills the vector it is given so a frame that keeps repainting the same cover
/// does not allocate one per frame.
fn snapshot_under(buffer: &Buffer, placement: &Placement, cells: &mut Vec<Cell>) {
    let area = buffer.area;
    cells.clear();
    for row in placement.row..placement.row.saturating_add(placement.rows) {
        for col in placement.col..placement.col.saturating_add(placement.cols) {
            let on_screen =
                row >= area.y && row < area.bottom() && col >= area.x && col < area.right();
            if on_screen {
                cells.push(buffer[(col, row)].clone());
            } else {
                // Off the screen: no frame draws there, now or before.
                cells.push(Cell::default());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Style};

    fn place(key: &str, row: u16, col: u16) -> Placement {
        Placement::new(key, row, col, 2, 4)
    }

    fn screen() -> Buffer {
        Buffer::empty(Rect::new(0, 0, 12, 8))
    }

    /// Fill a rectangle the way a selection bar does: same symbols, different
    /// background. The text diff writes every one of those cells.
    fn paint_bar(buffer: &mut Buffer, row: u16) {
        buffer.set_style(
            Rect::new(0, row, 12, 2),
            Style::default().bg(Color::Magenta),
        );
    }

    #[test]
    fn a_bar_written_over_a_cover_repaints_it() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        let cover = place("a", 0, 0);
        painter.plan(std::slice::from_ref(&cover));

        let mut buffer = screen();
        let first = painter.repaint_damaged(&buffer);
        assert!(first.is_empty(), "nothing has landed on the image yet");

        paint_bar(&mut buffer, 0);
        let plan = painter.repaint_damaged(&buffer);
        assert_eq!(plan.paints.len(), 1, "the bar wrote over the cover");
        assert!(plan.erases.is_empty(), "the image did not move");
    }

    #[test]
    fn an_undamaged_frame_repaints_nothing() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 0, 0)]);
        let buffer = screen();
        painter.repaint_damaged(&buffer);
        assert!(
            painter.repaint_damaged(&buffer).is_empty(),
            "a still screen must cost zero bytes"
        );
    }

    #[test]
    fn text_written_beside_a_cover_leaves_it_alone() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 0, 0)]);
        let mut buffer = screen();
        painter.repaint_damaged(&buffer);

        buffer.set_style(Rect::new(6, 0, 6, 2), Style::default().fg(Color::Red));
        assert!(painter.repaint_damaged(&buffer).is_empty());
    }

    #[test]
    fn a_cover_painted_this_frame_is_not_repainted() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        // The image goes up after the text is drawn, so the cells under it are
        // irrelevant to what the user sees.
        painter.plan(&[place("a", 0, 0)]);
        assert!(painter.repaint_damaged(&screen()).is_empty());
    }

    #[test]
    fn kitty_images_survive_text_written_under_them() {
        let mut painter = Painter::new(ArtProtocol::Kitty);
        painter.plan(&[place("a", 0, 0)]);
        let mut buffer = screen();
        painter.repaint_damaged(&buffer);
        paint_bar(&mut buffer, 0);
        // kitty images are objects above the grid: the bar goes behind them.
        assert!(painter.repaint_damaged(&buffer).is_empty());
    }

    #[test]
    fn a_cover_that_leaves_the_screen_stops_being_watched() {
        let mut painter = Painter::new(ArtProtocol::Sixel);
        painter.plan(&[place("a", 0, 0)]);
        let mut buffer = screen();
        painter.repaint_damaged(&buffer);
        painter.plan(&[]);

        // The snapshot must go with the placement, or the next image in that
        // rectangle would be compared against a stranger's cells.
        paint_bar(&mut buffer, 0);
        painter.plan(&[place("b", 4, 0)]);
        assert!(painter.repaint_damaged(&buffer).is_empty());
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
