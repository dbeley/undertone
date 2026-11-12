//! Subsonic/Navidrome REST client.
//!
//! Verified against Navidrome 0.64.2 (OpenSubsonic enabled) on
//! demo.navidrome.org. Everything here is transport, types and endpoints; no
//! UI concerns leak in.

mod cache;
mod client;
mod endpoints;
mod types;

pub use cache::TtlCache;
pub use client::{ServerInfo, SubsonicClient, SubsonicConfig, SubsonicError};
pub use endpoints::{AlbumListQuery, AlbumListType, MAX_ALBUM_PAGE};
pub use types::*;
