//! Undertone — a Subsonic/Navidrome client for very weak Linux hardware.

mod cli;

use clap::Parser;

fn main() {
    let args = cli::Args::parse();
    if args.version {
        println!("undertone {}", env!("CARGO_PKG_VERSION"));
        return;
    }
    println!(
        "undertone {} — early development (server: {}, art: {})",
        env!("CARGO_PKG_VERSION"),
        args.server.as_deref().unwrap_or("<from config>"),
        args.art.as_deref().unwrap_or("auto"),
    );
}
