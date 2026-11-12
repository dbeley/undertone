//! Command-line interface.

use clap::Parser;

/// A Subsonic/Navidrome client for very weak Linux hardware.
#[derive(Debug, Parser)]
#[command(name = "undertone", version, about)]
pub struct Args {
    /// Print the version and exit.
    #[arg(long)]
    pub version: bool,

    /// Subsonic server URL, overriding the config file.
    #[arg(long, env = "UNDERTONE_SERVER")]
    pub server: Option<String>,

    /// Config file path, overriding the XDG default.
    #[arg(long, short = 'c', env = "UNDERTONE_CONFIG")]
    pub config: Option<String>,

    /// Force an art backend: auto, sixel, kitty, symbols, off.
    #[arg(long)]
    pub art: Option<String>,

    /// Disable album art entirely.
    #[arg(long)]
    pub no_art: bool,

    /// Enable debug logging to the state directory.
    #[arg(long)]
    pub debug: bool,
}
