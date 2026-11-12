//! Command-line interface.

use clap::Parser;

/// A Subsonic/Navidrome client for very weak Linux hardware.
#[derive(Debug, Parser)]
#[command(name = "undertone", version, about)]
pub struct Args {
    /// Subsonic server URL, overriding the config file.
    #[arg(long, env = "UNDERTONE_SERVER")]
    pub server: Option<String>,

    /// Subsonic username, overriding the config file.
    #[arg(long, env = "UNDERTONE_USERNAME")]
    pub username: Option<String>,

    /// Config file path, overriding the XDG default.
    #[arg(long, short = 'c', env = "UNDERTONE_CONFIG")]
    pub config: Option<String>,

    /// Force an art backend: auto, sixel, kitty, symbols, off.
    #[arg(long)]
    pub art: Option<String>,

    /// Disable album art entirely.
    #[arg(long)]
    pub no_art: bool,

    /// Audio backend: mpv (default) or null (no sound, for testing).
    #[arg(long, default_value = "mpv")]
    pub player: String,

    /// Enable debug logging to the state directory.
    #[arg(long)]
    pub debug: bool,
}
