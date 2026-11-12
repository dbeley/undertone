//! Undertone — a Subsonic/Navidrome client for very weak Linux hardware.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use clap::Parser;
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::Terminal;

use undertone::app::App;
use undertone::art::{ArtService, SubsonicImageSource};
use undertone::cli;
use undertone::config::{CliOverrides, Config, Settings};
use undertone::event::{Events, Input};
use undertone::listen_later::ListenLater;
use undertone::player::{MpvOptions, MpvPlayer, NullPlayer, Player};
use undertone::render::{ArtProtocol, Caps, CellSize, ImageSource};
use undertone::session::Session;
use undertone::subsonic::{SubsonicClient, SubsonicConfig};
use undertone::ui;

/// Event poll interval. Fast enough that art appears promptly; the app only
/// redraws when something actually changed.
const TICK: Duration = Duration::from_millis(250);

fn main() {
    let args = cli::Args::parse();
    if let Err(err) = run(args) {
        eprintln!("undertone: {err}");
        std::process::exit(1);
    }
}

fn run(args: cli::Args) -> Result<(), Box<dyn std::error::Error>> {
    let env = |key: &str| std::env::var(key).ok();
    // Startup problems that degrade a feature instead of stopping the app are
    // collected here and shown in the status line once the UI exists.
    let mut startup_warnings: Vec<String> = Vec::new();

    let config_path = args.config.as_deref().map(std::path::Path::new);
    let (config, loaded) = Config::load(config_path, &env)?;
    let settings = config.resolve(
        CliOverrides {
            server: args.server.clone(),
            username: args.username.clone(),
            art: args.art.clone(),
        },
        &env,
    )?;

    let client = SubsonicClient::new(SubsonicConfig {
        server: settings.server.clone(),
        username: settings.username.clone(),
        password: settings.password.clone(),
    });

    let server_info = match client.ping() {
        Ok(info) => Some(info),
        Err(_err) => {
            // A server you cannot reach must not stop the app: there is still a
            // restored queue to look at and a config to fix, and quitting here
            // would make the failure look like a crash.
            //
            // The message is short on purpose: the error chain is long enough
            // to push every other warning off the status line.
            startup_warnings.push(format!("cannot reach {} — R to retry", settings.server));
            None
        }
    };

    let mut protocol = resolve_protocol(&args, &settings);
    let caps = Caps {
        protocol,
        cell: CellSize::detect(),
        truecolor: std::env::var("COLORTERM")
            .map(|value| value.contains("truecolor") || value.contains("24bit"))
            .unwrap_or(false),
    };

    let make_source = |protocol: ArtProtocol| -> Option<Arc<dyn ImageSource>> {
        if protocol.is_pixel() {
            // Ask the server for pre-scaled art so the weak device never
            // decodes a full-size image.
            Some(Arc::new(SubsonicImageSource::new(client.clone())))
        } else {
            None
        }
    };

    let mut art = ArtService::from_settings(
        protocol,
        &settings.cache_dir,
        settings.cache_bytes,
        make_source(protocol),
    );
    if art.is_err() {
        // An unusable cache directory is a reason to lose art, not to lose the
        // player. Retry somewhere writable with art switched off.
        startup_warnings.push(format!(
            "art cache unusable ({}) — art off",
            settings.cache_dir.display()
        ));
        protocol = ArtProtocol::Off;
        art = ArtService::from_settings(
            ArtProtocol::Off,
            &std::env::temp_dir().join("undertone-art"),
            1 << 20,
            None,
        );
    }
    let art = art?;

    let player: Box<dyn Player> = if args.player == "null" {
        Box::new(NullPlayer::new())
    } else {
        let options = MpvOptions {
            binary: settings.mpv_binary.clone(),
            ..Default::default()
        };
        match MpvPlayer::spawn(&options) {
            Ok(player) => Box::new(player),
            Err(_err) => {
                // mpv missing or refusing to start: browsing still works, and
                // saying so beats exiting with a stack of context.
                startup_warnings.push("mpv unavailable — playback disabled".to_string());
                Box::new(NullPlayer::new())
            }
        }
    };

    // Listen later lives in the state directory: it is user data, not cache,
    // and must survive a cache wipe.
    let listen_later = match state_dir() {
        Some(dir) => {
            let _ = std::fs::create_dir_all(&dir);
            ListenLater::load(dir.join("listen_later.json"))
        }
        None => ListenLater::load("/tmp/undertone-listen-later.json"),
    };
    let mut app = App::new(client, settings, player, art, protocol, 70, listen_later);
    if let Some(info) = server_info.as_ref() {
        app.status(format!(
            "{} {} · art {} · cell {}x{}",
            info.server_type.as_deref().unwrap_or("subsonic"),
            info.version,
            protocol.label(),
            caps.cell.width,
            caps.cell.height
        ));
    }
    // Skip the fetch when the server did not answer: a second failure message
    // would bury the one that says which URL was tried.
    if server_info.is_some() {
        app.load_albums();
    }

    // Restore what was playing last time. Done after the status line so the
    // "restored" message is the one you actually see.
    if let Some(dir) = state_dir() {
        if let Some(session) = Session::load(dir.join("session.json")) {
            app.restore_session(&session);
        }
    }

    // Warnings last so a degraded start is what you see. Each of these used to
    // be a hard failure; the messages are the only evidence the app is running
    // without something it asked for.
    if !startup_warnings.is_empty() {
        app.error(startup_warnings.join(" · "));
    } else if let Some(path) = loaded {
        app.status(format!("config: {}", path.display()));
    }

    let mut terminal = setup_terminal()?;
    let key_log = if args.debug { open_key_log() } else { None };
    let result = event_loop(&mut terminal, &mut app, key_log);
    // Save before tearing the terminal down: a failure here must not stop the
    // app from exiting cleanly, so the error is dropped rather than propagated.
    if let Some(dir) = state_dir() {
        let _ = app.session_snapshot().save(dir.join("session.json"));
    }
    restore_terminal(&mut terminal)?;
    app.art.shutdown();
    result
}

/// Append-only key log for `--debug`.
fn open_key_log() -> Option<std::fs::File> {
    let dir = state_dir()?;
    std::fs::create_dir_all(&dir).ok()?;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("keys.log"))
        .ok()
}

/// `$XDG_STATE_HOME/undertone`, where things that must survive a restart live.
fn state_dir() -> Option<std::path::PathBuf> {
    let base = match std::env::var("XDG_STATE_HOME") {
        Ok(value) if !value.is_empty() => value,
        _ => format!(
            "{}/.local/state",
            std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string())
        ),
    };
    Some(std::path::PathBuf::from(base).join("undertone"))
}

fn resolve_protocol(args: &cli::Args, settings: &Settings) -> ArtProtocol {
    if args.no_art {
        return ArtProtocol::Off;
    }
    if let Some(mode) = args.art.as_deref().and_then(ArtProtocol::parse) {
        return mode;
    }
    if let Some(mode) = ArtProtocol::parse(&settings.art_mode) {
        return mode;
    }
    Caps::detect(None).protocol
}

fn setup_terminal() -> Result<ui::Term, Box<dyn std::error::Error>> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;

    // Restore the terminal even if we panic, or the shell comes back broken.
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = disable_raw_mode();
        let _ = execute!(std::io::stdout(), LeaveAlternateScreen);
        previous_hook(info);
    }));

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    Ok(terminal)
}

fn restore_terminal(terminal: &mut ui::Term) -> Result<(), Box<dyn std::error::Error>> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    terminal.backend_mut().flush()?;
    Ok(())
}

fn event_loop(
    terminal: &mut ui::Term,
    app: &mut App,
    mut key_log: Option<std::fs::File>,
) -> Result<(), Box<dyn std::error::Error>> {
    let events = Events::spawn(TICK);

    loop {
        match events.next() {
            Some(Input::Key(key)) => {
                if let Some(log) = key_log.as_mut() {
                    // `--debug`: record every key so a phantom keypress can be
                    // traced to the terminal or to the app.
                    let _ = writeln!(
                        log,
                        "{:?} kind={:?} mods={:?}",
                        key.code, key.kind, key.modifiers
                    );
                    let _ = log.flush();
                }
                app.handle_key(key)
            }
            Some(Input::Resize) => {
                // Placements are invalid after a resize.
                app.needs_clear = true;
                app.needs_redraw = true;
            }
            Some(Input::Tick) => {
                if let Some(log) = key_log.as_mut() {
                    let _ = writeln!(log, "tick");
                }
                app.tick()
            }
            None => break,
        }

        app.pump();

        if app.should_quit {
            break;
        }

        let art_pending = app.art.pending() > 0;
        if !app.needs_redraw && !art_pending {
            continue;
        }
        app.needs_redraw = false;

        let size = terminal.size()?;
        // A zero-sized terminal (early resize, odd pty) would panic ratatui.
        if size.width == 0 || size.height == 0 {
            continue;
        }

        // Blanking cells does not remove a sixel: the pixels live in the
        // terminal's image layer, not the character grid. Navigation therefore
        // clears the screen instead of diffing it.
        if app.needs_clear {
            app.needs_clear = false;
            app.painter.invalidate();
            terminal.clear()?;
            if app.protocol == ArtProtocol::Kitty {
                // kitty images are objects, not cells, so delete them by id.
                terminal
                    .backend_mut()
                    .write_all(b"\x1b_Ga=d,d=a,q=2\x1b\\")?;
                terminal.backend_mut().flush()?;
            }
        }

        let area = Rect::new(0, 0, size.width, size.height);

        ui::collect_placements(app, area);
        let plan = app.painter.plan(&app.placements);

        if let Some(log) = key_log.as_mut() {
            let _ = writeln!(
                log,
                "draw placements={} paints={} pending={} ready={}",
                app.placements.len(),
                plan.paints.len(),
                app.art.pending(),
                app.art.ready_count()
            );
        }

        // Order matters: erase, draw text, then paint images over it.
        ui::emit_erases(terminal, &plan, app.protocol, app.settings.backdrop)?;
        terminal.draw(|frame| ui::draw(frame, app))?;
        ui::emit_paints(terminal, &plan, &app.art)?;
    }

    Ok(())
}
