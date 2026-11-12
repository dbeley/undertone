//! Playback queue: order, shuffle, repeat.
//!
//! Invariants held by every mutating method:
//!
//! * `order` is always a permutation of `0..items.len()`.
//! * `pos`, when set, indexes `order`, never `items`.
//! * The currently playing track keeps playing across shuffle, unshuffle,
//!   reordering and removal of other tracks.

use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Self {
        match self {
            Self::Off => Self::All,
            Self::All => Self::One,
            Self::One => Self::Off,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::All => "all",
            Self::One => "one",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct QueueItem {
    pub id: String,
    pub title: String,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub cover_art: Option<String>,
    pub duration: Option<u32>,
}

/// Result of moving through the queue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The current track did not change (repeat-one).
    Stayed,
    /// The current track changed.
    Advanced,
    /// The queue has no further tracks to play.
    Finished,
}

#[derive(Debug, Clone, Default)]
pub struct Queue {
    items: Vec<QueueItem>,
    /// Permutation of item indices, in playback order.
    order: Vec<usize>,
    /// Index into `order`.
    pos: Option<usize>,
    shuffle: bool,
    repeat: Repeat,
    seed: u64,
}

impl Queue {
    pub fn new() -> Self {
        Self {
            seed: seed_from_time(),
            ..Default::default()
        }
    }

    /// Deterministic constructor for tests.
    pub fn with_seed(seed: u64) -> Self {
        Self {
            seed,
            ..Default::default()
        }
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn repeat(&self) -> Repeat {
        self.repeat
    }

    pub fn set_repeat(&mut self, repeat: Repeat) {
        self.repeat = repeat;
    }

    pub fn cycle_repeat(&mut self) -> Repeat {
        self.repeat = self.repeat.next();
        self.repeat
    }

    pub fn is_shuffled(&self) -> bool {
        self.shuffle
    }

    /// Position within the playback order, if anything is playing.
    pub fn position(&self) -> Option<usize> {
        self.pos
    }

    /// Canonical (unshuffled) items.
    pub fn items(&self) -> &[QueueItem] {
        &self.items
    }

    /// Items in playback order.
    pub fn playback_items(&self) -> Vec<&QueueItem> {
        self.order.iter().map(|&i| &self.items[i]).collect()
    }

    pub fn current(&self) -> Option<&QueueItem> {
        let pos = self.pos?;
        let item = *self.order.get(pos)?;
        self.items.get(item)
    }

    pub fn current_item_index(&self) -> Option<usize> {
        let pos = self.pos?;
        self.order.get(pos).copied()
    }

    /// Replace the queue and start at the first track.
    pub fn replace(&mut self, items: Vec<QueueItem>) {
        self.items = items;
        self.rebuild_order();
        self.pos = if self.order.is_empty() { None } else { Some(0) };
    }

    /// Add tracks after the current ones without disturbing playback.
    pub fn append(&mut self, items: Vec<QueueItem>) {
        if items.is_empty() {
            return;
        }
        let base = self.items.len();
        self.items.extend(items);
        self.order.extend(base..self.items.len());
        if self.pos.is_none() && !self.order.is_empty() {
            self.pos = Some(0);
        }
    }

    pub fn clear(&mut self) {
        self.items.clear();
        self.order.clear();
        self.pos = None;
    }

    /// Jump to a position in the playback order.
    pub fn jump_to(&mut self, order_index: usize) -> Option<&QueueItem> {
        if order_index >= self.order.len() {
            return None;
        }
        self.pos = Some(order_index);
        self.current()
    }

    pub fn advance(&mut self) -> Step {
        let Some(pos) = self.pos else {
            if self.order.is_empty() {
                return Step::Finished;
            }
            self.pos = Some(0);
            return Step::Advanced;
        };
        if self.repeat == Repeat::One {
            return Step::Stayed;
        }
        if pos + 1 < self.order.len() {
            self.pos = Some(pos + 1);
            return Step::Advanced;
        }
        if self.repeat == Repeat::All {
            self.pos = Some(0);
            return Step::Advanced;
        }
        Step::Finished
    }

    pub fn retreat(&mut self) -> Step {
        let Some(pos) = self.pos else {
            if self.order.is_empty() {
                return Step::Finished;
            }
            self.pos = Some(0);
            return Step::Advanced;
        };
        if pos > 0 {
            self.pos = Some(pos - 1);
            return Step::Advanced;
        }
        if self.repeat == Repeat::All {
            self.pos = Some(self.order.len() - 1);
            return Step::Advanced;
        }
        Step::Stayed
    }

    /// Enable or disable shuffle, keeping the current track playing.
    pub fn set_shuffle(&mut self, enabled: bool) {
        if enabled == self.shuffle {
            return;
        }
        self.shuffle = enabled;
        let current_item = self.current_item_index();
        if enabled {
            let mut rest: Vec<usize> = (0..self.items.len())
                .filter(|&i| Some(i) != current_item)
                .collect();
            shuffle(&mut rest, &mut self.seed);
            self.order = current_item.into_iter().chain(rest).collect();
            self.pos = if self.order.is_empty() { None } else { Some(0) };
        } else {
            self.order = (0..self.items.len()).collect();
            self.pos = current_item;
        }
    }

    pub fn toggle_shuffle(&mut self) -> bool {
        self.set_shuffle(!self.shuffle);
        self.shuffle
    }

    /// Remove an item by its position in the playback order.
    pub fn remove(&mut self, order_index: usize) -> Option<QueueItem> {
        if order_index >= self.order.len() {
            return None;
        }
        let item_index = self.order[order_index];
        let removed = self.items.remove(item_index);
        self.order.remove(order_index);
        for entry in &mut self.order {
            if *entry > item_index {
                *entry -= 1;
            }
        }
        if let Some(pos) = self.pos {
            if order_index < pos {
                self.pos = Some(pos - 1);
            } else if order_index == pos && pos >= self.order.len() {
                self.pos = if self.order.is_empty() {
                    None
                } else {
                    Some(self.order.len() - 1)
                };
            }
        }
        Some(removed)
    }

    /// Move an item within the playback order, keeping the current track.
    pub fn move_item(&mut self, from: usize, to: usize) -> bool {
        if from >= self.order.len() || to >= self.order.len() || from == to {
            return false;
        }
        let current_item = self.current_item_index();
        let entry = self.order.remove(from);
        self.order.insert(to, entry);
        if let Some(current_item) = current_item {
            self.pos = self.order.iter().position(|&i| i == current_item);
        }
        true
    }

    fn rebuild_order(&mut self) {
        self.order = (0..self.items.len()).collect();
        if self.shuffle {
            shuffle(&mut self.order, &mut self.seed);
        }
    }
}

fn seed_from_time() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0x9E37_79B9_7F4A_7C15)
        | 1
}

/// xorshift64*, enough for shuffling a playlist without pulling in `rand`.
fn next_random(state: &mut u64) -> u64 {
    let mut x = *state;
    x ^= x >> 12;
    x ^= x << 25;
    x ^= x >> 27;
    *state = x;
    x.wrapping_mul(0x2545_F491_4F6C_DD1D)
}

fn shuffle<T>(values: &mut [T], state: &mut u64) {
    if values.len() < 2 {
        return;
    }
    for i in (1..values.len()).rev() {
        let j = (next_random(state) % (i as u64 + 1)) as usize;
        values.swap(i, j);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            title: format!("track {id}"),
            ..Default::default()
        }
    }

    fn queue_of(ids: &[&str]) -> Queue {
        let mut queue = Queue::with_seed(42);
        queue.replace(ids.iter().map(|id| item(id)).collect());
        queue
    }

    fn current_id(queue: &Queue) -> Option<&str> {
        queue.current().map(|i| i.id.as_str())
    }

    /// The invariant every mutation must preserve.
    fn assert_permutation(queue: &Queue) {
        let mut sorted = queue.order.clone();
        sorted.sort_unstable();
        assert_eq!(
            sorted,
            (0..queue.items.len()).collect::<Vec<_>>(),
            "order must be a permutation of item indices"
        );
    }

    #[test]
    fn replace_starts_at_the_first_track() {
        let queue = queue_of(&["a", "b", "c"]);
        assert_eq!(queue.len(), 3);
        assert_eq!(current_id(&queue), Some("a"));
        assert_eq!(queue.position(), Some(0));
        assert_permutation(&queue);
    }

    #[test]
    fn empty_queue_reports_no_current_track() {
        let queue = Queue::new();
        assert!(queue.is_empty());
        assert_eq!(queue.current(), None);
        assert_eq!(queue.position(), None);
    }

    #[test]
    fn advance_walks_forward_then_finishes() {
        let mut queue = queue_of(&["a", "b"]);
        assert_eq!(queue.advance(), Step::Advanced);
        assert_eq!(current_id(&queue), Some("b"));
        assert_eq!(queue.advance(), Step::Finished);
        // Finishing does not wrap and does not lose the current track.
        assert_eq!(current_id(&queue), Some("b"));
    }

    #[test]
    fn repeat_all_wraps_both_ways() {
        let mut queue = queue_of(&["a", "b"]);
        queue.set_repeat(Repeat::All);
        assert_eq!(queue.advance(), Step::Advanced);
        assert_eq!(queue.advance(), Step::Advanced);
        assert_eq!(current_id(&queue), Some("a"));
        assert_eq!(queue.retreat(), Step::Advanced);
        assert_eq!(current_id(&queue), Some("b"));
    }

    #[test]
    fn repeat_one_stays_put() {
        let mut queue = queue_of(&["a", "b"]);
        queue.set_repeat(Repeat::One);
        assert_eq!(queue.advance(), Step::Stayed);
        assert_eq!(current_id(&queue), Some("a"));
    }

    #[test]
    fn previous_at_the_start_holds_position_when_repeat_is_off() {
        let mut queue = queue_of(&["a", "b"]);
        assert_eq!(queue.retreat(), Step::Stayed);
        assert_eq!(current_id(&queue), Some("a"));
    }

    #[test]
    fn repeat_cycles_off_all_one() {
        let mut queue = Queue::new();
        assert_eq!(queue.repeat(), Repeat::Off);
        assert_eq!(queue.cycle_repeat(), Repeat::All);
        assert_eq!(queue.cycle_repeat(), Repeat::One);
        assert_eq!(queue.cycle_repeat(), Repeat::Off);
    }

    #[test]
    fn shuffle_keeps_every_track_and_the_current_one() {
        let mut queue = queue_of(&["a", "b", "c", "d", "e"]);
        let before = current_id(&queue).unwrap().to_string();
        queue.set_shuffle(true);
        assert!(queue.is_shuffled());
        assert_eq!(current_id(&queue), Some(before.as_str()));
        assert_permutation(&queue);
        assert_eq!(queue.len(), 5);
        let mut ids: Vec<&str> = queue
            .playback_items()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, vec!["a", "b", "c", "d", "e"]);
    }

    #[test]
    fn unshuffle_restores_canonical_order_and_keeps_current() {
        let mut queue = queue_of(&["a", "b", "c", "d"]);
        queue.advance();
        queue.advance();
        let current = current_id(&queue).unwrap().to_string();
        queue.set_shuffle(true);
        queue.set_shuffle(false);
        assert!(!queue.is_shuffled());
        let ids: Vec<&str> = queue
            .playback_items()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b", "c", "d"]);
        assert_eq!(current_id(&queue), Some(current.as_str()));
    }

    #[test]
    fn appending_does_not_disturb_playback() {
        let mut queue = queue_of(&["a", "b"]);
        queue.advance();
        queue.append(vec![item("c"), item("d")]);
        assert_eq!(queue.len(), 4);
        assert_eq!(current_id(&queue), Some("b"));
        assert_eq!(queue.advance(), Step::Advanced);
        assert_eq!(current_id(&queue), Some("c"));
        assert_permutation(&queue);
    }

    #[test]
    fn append_into_an_empty_queue_starts_playback() {
        let mut queue = Queue::new();
        queue.append(vec![item("a")]);
        assert_eq!(current_id(&queue), Some("a"));
    }

    #[test]
    fn removing_a_later_track_keeps_current() {
        let mut queue = queue_of(&["a", "b", "c"]);
        assert!(queue.remove(2).is_some());
        assert_eq!(queue.len(), 2);
        assert_eq!(current_id(&queue), Some("a"));
        assert_permutation(&queue);
    }

    #[test]
    fn removing_the_current_track_advances_to_the_next() {
        let mut queue = queue_of(&["a", "b", "c"]);
        queue.remove(0);
        assert_eq!(queue.len(), 2);
        assert_eq!(current_id(&queue), Some("b"));
        assert_permutation(&queue);
    }

    #[test]
    fn removing_the_last_remaining_track_empties_the_queue() {
        let mut queue = queue_of(&["a"]);
        queue.remove(0);
        assert!(queue.is_empty());
        assert_eq!(queue.current(), None);
        assert_eq!(queue.position(), None);
    }

    #[test]
    fn removing_the_current_tail_clamps_position() {
        let mut queue = queue_of(&["a", "b"]);
        queue.advance();
        queue.remove(1);
        assert_eq!(current_id(&queue), Some("a"));
        assert_eq!(queue.position(), Some(0));
        assert_permutation(&queue);
    }

    #[test]
    fn moving_an_item_keeps_the_current_track_playing() {
        let mut queue = queue_of(&["a", "b", "c", "d"]);
        queue.advance();
        queue.advance();
        assert_eq!(current_id(&queue), Some("c"));
        assert!(queue.move_item(2, 0));
        let ids: Vec<&str> = queue
            .playback_items()
            .iter()
            .map(|i| i.id.as_str())
            .collect();
        assert_eq!(ids, vec!["c", "a", "b", "d"]);
        assert_eq!(current_id(&queue), Some("c"));
        assert_eq!(queue.position(), Some(0));
        assert_permutation(&queue);
    }

    #[test]
    fn jumping_out_of_range_is_a_no_op() {
        let mut queue = queue_of(&["a", "b"]);
        assert!(queue.jump_to(9).is_none());
        assert_eq!(current_id(&queue), Some("a"));
    }

    #[test]
    fn jumping_to_a_position_plays_that_track() {
        let mut queue = queue_of(&["a", "b", "c"]);
        let item = queue.jump_to(2).map(|i| i.id.clone());
        assert_eq!(item.as_deref(), Some("c"));
        assert_eq!(queue.position(), Some(2));
    }

    #[test]
    fn clear_resets_everything() {
        let mut queue = queue_of(&["a", "b"]);
        queue.clear();
        assert!(queue.is_empty());
        assert_eq!(queue.position(), None);
        assert_eq!(queue.advance(), Step::Finished);
    }

    #[test]
    fn long_queue_survives_a_shuffle_unshuffle_round_trip() {
        let ids: Vec<String> = (0..200).map(|i| i.to_string()).collect();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let mut queue = queue_of(&refs);
        queue.jump_to(100);
        let current = current_id(&queue).unwrap().to_string();
        queue.set_shuffle(true);
        queue.set_shuffle(false);
        assert_eq!(current_id(&queue), Some(current.as_str()));
        assert_eq!(queue.len(), 200);
        assert_permutation(&queue);
    }
}
