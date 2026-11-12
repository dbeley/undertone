//! Playback abstraction.
//!
//! The player is command-and-event based: the app issues commands, and applies
//! events to its own state. There is no shared mutable playback state inside
//! the player, which keeps the data flow one-directional and the whole thing
//! testable without audio hardware.

mod mpv;
mod null;
mod protocol;
mod scrobble;

pub use mpv::{MpvOptions, MpvPlayer};
pub use null::NullPlayer;
pub use scrobble::{ScrobbleAction, Scrobbler};

#[derive(Debug, thiserror::Error)]
pub enum PlayerError {
    #[error("could not start mpv: {0}")]
    Spawn(String),
    #[error("player ipc failed: {0}")]
    Ipc(String),
    #[error("player is not connected")]
    Disconnected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadMode {
    /// Replace the playlist with these tracks.
    Replace,
    /// Append these tracks to the current playlist (keeps playback gapless).
    Append,
}

/// One track handed to the player.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrackSource {
    /// Authenticated stream URL.
    pub url: String,
    /// Shown by mpv as `media-title`; useful for debugging.
    pub title: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seek {
    Absolute,
    Relative,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndReason {
    /// Played to the end.
    Eof,
    /// Explicit stop.
    Stop,
    /// Could not play (bad URL, unsupported codec, network).
    Error,
    /// Server redirected to something unplayable.
    Redirect,
    Quit,
    Unknown,
}

/// What the player reports back.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    FileLoaded,
    Duration(f64),
    Position(f64),
    Paused(bool),
    Volume(f64),
    /// The playlist finished and mpv is idle.
    Idle,
    Ended(EndReason),
    /// The subprocess or IPC channel died.
    Crashed(String),
}

/// Commands the app can send to a player backend.
pub trait Player: Send {
    fn load(&mut self, tracks: &[TrackSource], mode: LoadMode) -> Result<(), PlayerError>;
    fn play(&mut self) -> Result<(), PlayerError>;
    fn pause(&mut self, paused: bool) -> Result<(), PlayerError>;
    fn stop(&mut self) -> Result<(), PlayerError>;
    fn seek(&mut self, mode: Seek, seconds: f64) -> Result<(), PlayerError>;
    fn set_volume(&mut self, percent: u8) -> Result<(), PlayerError>;
    fn set_mute(&mut self, muted: bool) -> Result<(), PlayerError>;
    fn next(&mut self) -> Result<(), PlayerError>;
    fn previous(&mut self) -> Result<(), PlayerError>;

    /// Take every event received since the last call. Never blocks.
    fn drain_events(&mut self) -> Vec<PlayerEvent>;

    /// Shut the backend down. Safe to call twice.
    fn shutdown(&mut self);
}
