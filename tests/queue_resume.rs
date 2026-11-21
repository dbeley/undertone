//! Queue edits against a player that records what it was asked to do.
//!
//! Reordering a playing queue rebuilds mpv's window, which restarts the stream.
//! The app has to take playback back to where it was, and it can only do that
//! once the new file is loaded: a seek sent before then is dropped.

use std::sync::{Arc, Mutex};

use undertone::app::{App, View};
use undertone::art::ArtService;
use undertone::config::{Palette, Settings};
use undertone::player::{LoadMode, Player, PlayerError, PlayerEvent, Seek, TrackSource};
use undertone::queue::QueueItem;
use undertone::render::{ArtProtocol, CellSize};
use undertone::subsonic::{SubsonicClient, SubsonicConfig};

/// Records seeks, and reports `FileLoaded` after a replacing load.
#[derive(Default)]
struct RecordingPlayer {
    seeks: Arc<Mutex<Vec<(Seek, f64)>>>,
    events: Vec<PlayerEvent>,
}

impl Player for RecordingPlayer {
    fn load(&mut self, _tracks: &[TrackSource], mode: LoadMode) -> Result<(), PlayerError> {
        if mode == LoadMode::Replace {
            self.events.push(PlayerEvent::FileLoaded);
        }
        Ok(())
    }

    fn play(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn pause(&mut self, _paused: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn seek(&mut self, mode: Seek, seconds: f64) -> Result<(), PlayerError> {
        self.seeks.lock().expect("seeks lock").push((mode, seconds));
        Ok(())
    }

    fn set_volume(&mut self, _percent: u8) -> Result<(), PlayerError> {
        Ok(())
    }

    fn set_mute(&mut self, _muted: bool) -> Result<(), PlayerError> {
        Ok(())
    }

    fn next(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn previous(&mut self) -> Result<(), PlayerError> {
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.events)
    }

    fn shutdown(&mut self) {}
}

fn app_with(player: RecordingPlayer) -> App {
    let dir = std::env::temp_dir().join(format!("undertone-queue-resume-{}", std::process::id()));
    let settings = Settings {
        server: "http://127.0.0.1:1".into(),
        username: "demo".into(),
        password: "demo".into(),
        scrobble: false,
        auto_dj: false,
        palette: Palette::Partial,
        backdrop: [0, 0, 0],
        cache_dir: dir.clone(),
        cache_bytes: 1 << 20,
        mpv_binary: std::path::PathBuf::from("mpv"),
    };
    let client = SubsonicClient::new(SubsonicConfig {
        server: settings.server.clone(),
        username: settings.username.clone(),
        password: settings.password.clone(),
    });
    let art = ArtService::from_settings(ArtProtocol::Symbols, &dir, 1 << 20, None).expect("art");
    App::new(
        client,
        settings,
        Box::new(player),
        art,
        ArtProtocol::Symbols,
        CellSize::FALLBACK,
        80,
        undertone::listen_later::ListenLater::load(std::env::temp_dir().join(format!(
            "undertone-queue-resume-ll-{}.json",
            std::process::id()
        ))),
    )
}

fn item(id: &str) -> QueueItem {
    QueueItem {
        id: id.to_string(),
        title: id.to_string(),
        ..Default::default()
    }
}

#[test]
fn reordering_the_queue_resumes_the_playing_track_where_it_was() {
    let seeks = Arc::new(Mutex::new(Vec::new()));
    let mut app = app_with(RecordingPlayer {
        seeks: Arc::clone(&seeks),
        ..Default::default()
    });
    app.queue.replace(vec![item("a"), item("b"), item("c")]);
    app.play_current();
    // Consume the initial `FileLoaded`: starting playback has no seek to make.
    app.pump();
    assert!(seeks.lock().unwrap().is_empty());

    // Where mpv last reported playback to be.
    app.now.position = 42.0;
    app.view = View::Queue;

    app.move_queue_item(1);

    assert!(
        seeks.lock().unwrap().is_empty(),
        "the seek must wait for the new file to load"
    );
    assert_eq!(app.now.position, 42.0, "the bar must not jump to 0:00");

    app.pump();

    assert_eq!(
        *seeks.lock().unwrap(),
        vec![(Seek::Absolute, 42.0)],
        "the track is taken back to where it was"
    );
    assert_eq!(app.now.position, 42.0);
    assert_eq!(
        app.now.track.as_ref().map(|t| t.id.as_str()),
        Some("a"),
        "reordering does not change the track"
    );
}
