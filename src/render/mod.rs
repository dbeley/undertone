//! Terminal graphics: capability detection, chafa encoding, caches, the
//! background worker, and the placement painter that reconciles what is on
//! screen with what should be.
//!
//! The target machine runs foot, which speaks sixel only. kitty graphics is
//! supported for ghostty/kitty on faster machines; symbols is the floor.

mod cache;
mod chafa;
pub mod clear;
mod images;
mod painter;
mod protocol;
mod worker;

pub use cache::BlobCache;
pub use chafa::{
    sixel_dimensions, split_sequences, ArtRenderer, ChafaRenderer, RenderError, RenderRequest,
};
pub use clear::{blank_sixel, kitty_delete, kitty_id, tag_kitty};
pub use images::ImageCache;
pub use painter::{FramePlan, PaintOp, Painter, Placement};
pub use protocol::{detect_protocol, ArtProtocol, Caps, CellSize};
pub use worker::{Done, ImageSource, Job, RenderWorker, PRIORITY_PREFETCH, PRIORITY_VISIBLE};
