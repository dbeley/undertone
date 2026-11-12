//! Undertone — a Subsonic/Navidrome client for very weak Linux hardware.
//!
//! The crate is split into a library (client, player, render pipeline, state)
//! and a thin binary, so integration tests can drive the real code paths.

pub mod cli;
pub mod subsonic;
