//! Application state and input handling.
//!
//! Deliberately free of terminal I/O so it can be tested: keys go in, state
//! changes, and `ui::draw` renders it. The only blocking calls are explicit
//! user actions (opening an album, changing sort), never the tick path.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent};

use crate::art::ArtService;
use crate::config::Settings;
use crate::player::{LoadMode, Player, PlayerEvent, ScrobbleAction, Scrobbler, Seek, TrackSource};
use crate::queue::{Queue, QueueItem, Step};
use crate::render::{ArtProtocol, Painter, Placement};
use crate::subsonic::{Album, AlbumListQuery, AlbumListType, Song, SubsonicClient};

/// Cells a thumbnail occupies in the album list.
pub const THUMB_CELLS: u16 = 4;
pub const THUMB_ROWS: u16 = 2;
/// Rows of text per album entry.
pub const ROW_HEIGHT: u16 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Albums,
    AlbumDetail,
    Queue,
    Help,
}

pub struct NowPlaying {
    pub track: Option<QueueItem>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    pub listened: f64,
    pub scrobbler: Scrobbler,
}

impl Default for NowPlaying {
    fn default() -> Self {
        Self {
            track: None,
            position: 0.0,
            duration: 0.0,
            paused: true,
            listened: 0.0,
            scrobbler: Scrobbler::default(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub text: String,
    pub at: Instant,
    pub error: bool,
}

pub struct App {
    pub client: SubsonicClient,
    pub settings: Settings,
    pub queue: Queue,
    pub player: Box<dyn Player>,
    pub art: ArtService,
    pub painter: Painter,
    pub protocol: ArtProtocol,

    pub view: View,
    pub albums: Vec<Album>,
    pub sort: AlbumListType,
    pub selected: usize,
    pub scroll: usize,
    pub detail: Option<Album>,
    pub detail_selected: usize,

    pub now: NowPlaying,
    pub status: Option<Status>,
    pub should_quit: bool,
    pub needs_redraw: bool,
    pub volume: u8,
    pub muted: bool,

    /// Filled during draw, consumed by the painter after it.
    pub placements: Vec<Placement>,
    pub last_tick: Instant,
    pub list_area: (u16, u16),
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: SubsonicClient,
        settings: Settings,
        player: Box<dyn Player>,
        art: ArtService,
        protocol: ArtProtocol,
        volume: u8,
    ) -> Self {
        let painter = Painter::new(protocol);
        Self {
            client,
            queue: Queue::new(),
            player,
            art,
            painter,
            protocol,
            settings,
            view: View::Albums,
            albums: Vec::new(),
            sort: AlbumListType::Random,
            selected: 0,
            scroll: 0,
            detail: None,
            detail_selected: 0,
            now: NowPlaying::default(),
            status: None,
            should_quit: false,
            needs_redraw: true,
            volume,
            muted: false,
            placements: Vec::new(),
            last_tick: Instant::now(),
            list_area: (80, 24),
        }
    }

    pub fn status(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            at: Instant::now(),
            error: false,
        });
        self.needs_redraw = true;
    }

    pub fn error(&mut self, text: impl Into<String>) {
        self.status = Some(Status {
            text: text.into(),
            at: Instant::now(),
            error: true,
        });
        self.needs_redraw = true;
    }

    // ---------------------------------------------------------------- data

    pub fn load_albums(&mut self) {
        let query = AlbumListQuery {
            kind: self.sort,
            size: 100,
            offset: 0,
            ..Default::default()
        };
        match self.client.album_list(&query) {
            Ok(albums) => {
                self.albums = albums;
                self.selected = 0;
                self.scroll = 0;
                self.needs_redraw = true;
            }
            Err(err) => self.error(format!("could not load albums: {err}")),
        }
    }

    pub fn cycle_sort(&mut self) {
        let order = AlbumListType::ALL_IN_UI;
        let index = order.iter().position(|k| *k == self.sort).unwrap_or(0);
        self.sort = order[(index + 1) % order.len()];
        let label = self.sort.as_str().to_string();
        self.load_albums();
        self.status(format!("sort: {label}"));
    }

    pub fn open_selected_album(&mut self) {
        let Some(album) = self.albums.get(self.selected) else {
            return;
        };
        let id = album.id.clone();
        let name = album.name.clone();
        match self.client.album(&id) {
            Ok(album) => {
                self.detail = Some(album);
                self.detail_selected = 0;
                self.view = View::AlbumDetail;
                self.needs_redraw = true;
                self.painter.invalidate();
            }
            Err(err) => {
                self.error(format!("could not open {name}: {err}"));
            }
        }
    }

    // ------------------------------------------------------------ playback

    fn stream_url(&self, song_id: &str) -> String {
        if self.settings.transcode {
            self.client.stream_url(song_id, Some(192), Some("mp3"))
        } else {
            self.client.stream_url(song_id, None, None)
        }
    }

    fn to_queue_items(songs: &[Song]) -> Vec<QueueItem> {
        songs
            .iter()
            .map(|song| QueueItem {
                id: song.id.clone(),
                title: song.title.clone(),
                artist: song.artist.clone(),
                album: song.album.clone(),
                cover_art: song.cover_art.clone(),
                duration: song.duration,
            })
            .collect()
    }

    /// Queue this album and start playing it.
    pub fn play_album(&mut self) {
        let Some(album) = self.detail.clone() else {
            return;
        };
        if album.song.is_empty() {
            self.error("album has no tracks");
            return;
        }
        let items = Self::to_queue_items(&album.song);
        self.queue.replace(items);
        self.status(format!("playing {}", album.name));
        self.play_current();
    }

    /// Play one track from the open album, queueing the rest of it.
    pub fn play_track_from_album(&mut self, index: usize) {
        let Some(album) = self.detail.clone() else {
            return;
        };
        if index >= album.song.len() {
            return;
        }
        let items = Self::to_queue_items(&album.song);
        self.queue.replace(items);
        self.queue.jump_to(index);
        self.play_current();
    }

    pub fn play_current(&mut self) {
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        let url = self.stream_url(&item.id);
        let loaded = self.player.load(
            &[TrackSource {
                url,
                title: item.title.clone(),
            }],
            LoadMode::Replace,
        );
        if let Err(err) = loaded {
            self.error(format!("player: {err}"));
            return;
        }
        let _ = self.player.play();
        self.now.track = Some(item.clone());
        self.now.position = 0.0;
        self.now.duration = f64::from(item.duration.unwrap_or(0));
        self.now.listened = 0.0;
        self.now.paused = false;
        self.needs_redraw = true;

        if self.settings.scrobble {
            let actions = self.now.scrobbler.start(&item.id, unix_millis());
            self.dispatch_scrobbles(actions);
        }
    }

    fn dispatch_scrobbles(&self, actions: Vec<ScrobbleAction>) {
        for action in actions {
            let client = self.client.clone();
            std::thread::spawn(move || match action {
                ScrobbleAction::NowPlaying { id } => {
                    let _ = client.scrobble(&id, false, None);
                }
                ScrobbleAction::Submit { id, started_at_ms } => {
                    let _ = client.scrobble(&id, true, Some(started_at_ms));
                }
            });
        }
    }

    pub fn toggle_pause(&mut self) {
        if self.now.track.is_none() {
            self.play_current();
            return;
        }
        self.now.paused = !self.now.paused;
        let _ = self.player.pause(self.now.paused);
        self.needs_redraw = true;
    }

    pub fn next_track(&mut self) {
        match self.queue.advance() {
            Step::Advanced => self.play_current(),
            Step::Stayed => self.play_current(),
            Step::Finished => self.status("end of queue"),
        }
        self.needs_redraw = true;
    }

    pub fn previous_track(&mut self) {
        if self.queue.retreat() == Step::Advanced {
            self.play_current();
        }
        self.needs_redraw = true;
    }

    pub fn seek(&mut self, seconds: f64) {
        let _ = self.player.seek(Seek::Relative, seconds);
        let target = (self.now.position + seconds).max(0.0);
        self.now.position = if self.now.duration > 0.0 {
            target.min(self.now.duration)
        } else {
            target
        };
        self.needs_redraw = true;
    }

    pub fn change_volume(&mut self, delta: i16) {
        let next = (i16::from(self.volume) + delta).clamp(0, 100) as u8;
        self.volume = next;
        let _ = self.player.set_volume(next);
        self.status(format!("volume {next}%"));
    }

    pub fn toggle_mute(&mut self) {
        self.muted = !self.muted;
        let _ = self.player.set_mute(self.muted);
        self.status(if self.muted { "muted" } else { "unmuted" });
    }

    pub fn toggle_shuffle(&mut self) {
        let on = self.queue.toggle_shuffle();
        self.status(format!("shuffle {}", if on { "on" } else { "off" }));
    }

    pub fn cycle_repeat(&mut self) {
        let mode = self.queue.cycle_repeat();
        self.status(format!("repeat {}", mode.label()));
    }

    // --------------------------------------------------------------- events

    /// Apply player events and the passage of time. Called on every tick and
    /// whenever the event loop wakes.
    pub fn pump(&mut self) {
        for event in self.player.drain_events() {
            match event {
                PlayerEvent::Position(position) => {
                    self.now.position = position;
                    self.needs_redraw = true;
                }
                PlayerEvent::Duration(duration) => {
                    self.now.duration = duration;
                    self.needs_redraw = true;
                }
                PlayerEvent::Paused(paused) => {
                    self.now.paused = paused;
                    self.needs_redraw = true;
                }
                PlayerEvent::Ended(reason) => {
                    if matches!(reason, crate::player::EndReason::Eof) {
                        match self.queue.advance() {
                            Step::Advanced | Step::Stayed => self.play_current(),
                            Step::Finished => {
                                self.now.paused = true;
                                self.status("end of queue");
                            }
                        }
                    }
                }
                PlayerEvent::Crashed(message) => {
                    self.error(format!("player crashed: {message}"));
                }
                PlayerEvent::FileLoaded | PlayerEvent::Volume(_) | PlayerEvent::Idle => {}
            }
        }

        if self.art.poll() {
            self.needs_redraw = true;
        }
    }

    /// Per-second accounting: progress and scrobble thresholds.
    pub fn tick(&mut self) {
        let elapsed = self.last_tick.elapsed().as_secs_f64();
        self.last_tick = Instant::now();
        if elapsed <= 0.0 {
            return;
        }

        let playing = self.now.track.is_some() && !self.now.paused;
        if !playing {
            return;
        }

        self.now.listened += elapsed;
        if self.settings.scrobble {
            let duration = if self.now.duration > 0.0 {
                Some(self.now.duration)
            } else {
                None
            };
            let actions = self.now.scrobbler.progress(self.now.listened, duration);
            if !actions.is_empty() {
                self.dispatch_scrobbles(actions);
            }
        }
        self.needs_redraw = true;
    }

    // ------------------------------------------------------------------ keys

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.needs_redraw = true;

        if self.view == View::Help {
            self.view = View::Albums;
            return;
        }

        match key.code {
            KeyCode::Char('q') if key.modifiers.is_empty() => self.should_quit = true,
            KeyCode::Esc => {
                if self.view == View::Albums {
                    self.should_quit = true;
                } else {
                    self.view = View::Albums;
                    self.painter.invalidate();
                }
            }
            KeyCode::Char('?') => self.view = View::Help,
            KeyCode::Char('Q') => {
                self.view = if self.view == View::Queue {
                    View::Albums
                } else {
                    View::Queue
                };
                self.painter.invalidate();
            }
            KeyCode::Char('j') | KeyCode::Down => self.move_cursor(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_cursor(-1),
            KeyCode::Char('g') => self.jump_cursor(0),
            KeyCode::Char('G') => self.jump_cursor(usize::MAX),
            KeyCode::Enter => self.activate(),
            KeyCode::Char(' ') => self.toggle_pause(),
            KeyCode::Char('>') | KeyCode::Char('.') => self.next_track(),
            KeyCode::Char('<') | KeyCode::Char(',') => self.previous_track(),
            KeyCode::Char('f') => self.seek(10.0),
            KeyCode::Char('b') => self.seek(-10.0),
            KeyCode::Char('+') | KeyCode::Char('=') => self.change_volume(5),
            KeyCode::Char('-') => self.change_volume(-5),
            KeyCode::Char('m') => self.toggle_mute(),
            KeyCode::Char('s') => self.toggle_shuffle(),
            KeyCode::Char('r') => self.cycle_repeat(),
            KeyCode::Char('S') => self.cycle_sort(),
            KeyCode::Char('R') => self.load_albums(),
            _ => {}
        }
    }

    fn cursor_len(&self) -> usize {
        match self.view {
            View::Albums => self.albums.len(),
            View::AlbumDetail => self
                .detail
                .as_ref()
                .map(|album| album.song.len())
                .unwrap_or(0),
            View::Queue => self.queue.len(),
            View::Help => 0,
        }
    }

    fn cursor(&self) -> usize {
        match self.view {
            View::Albums => self.selected,
            View::AlbumDetail => self.detail_selected,
            View::Queue => self.queue.position().unwrap_or(0),
            View::Help => 0,
        }
    }

    fn move_cursor(&mut self, delta: i32) {
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        let current = self.cursor() as i32;
        let next = (current + delta).clamp(0, len as i32 - 1) as usize;
        match self.view {
            View::Albums => self.selected = next,
            View::AlbumDetail => self.detail_selected = next,
            View::Queue => {
                self.queue.jump_to(next);
            }
            View::Help => {}
        }
    }

    fn jump_cursor(&mut self, target: usize) {
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        let next = target.min(len - 1);
        match self.view {
            View::Albums => self.selected = next,
            View::AlbumDetail => self.detail_selected = next,
            View::Queue => {
                self.queue.jump_to(next);
            }
            View::Help => {}
        }
    }

    fn activate(&mut self) {
        match self.view {
            View::Albums => {
                if !self.albums.is_empty() {
                    self.open_selected_album();
                }
            }
            View::AlbumDetail => {
                if self.detail.is_some() {
                    if self.detail_selected == 0 {
                        self.play_album();
                    } else {
                        // Row 0 is the cover, so track indices start at 1.
                        self.play_track_from_album(self.detail_selected - 1);
                    }
                }
            }
            View::Queue => {
                self.play_current();
            }
            View::Help => {}
        }
    }

    /// Keep the selection inside the visible window.
    pub fn ensure_visible(&mut self, visible_rows: usize) {
        let visible = (visible_rows / ROW_HEIGHT as usize).max(1);
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + visible {
            self.scroll = self.selected + 1 - visible;
        }
    }
}

fn unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// How long a status message stays on screen.
pub const STATUS_TTL: Duration = Duration::from_secs(5);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Settings;
    use crate::player::NullPlayer;
    use crossterm::event::KeyModifiers;
    use std::path::PathBuf;

    fn settings() -> Settings {
        Settings {
            server: "https://demo.navidrome.org".into(),
            username: "demo".into(),
            password: "demo".into(),
            scrobble: false,
            auto_dj: false,
            art_mode: "off".into(),
            thumb_colors: 16,
            cover_colors: 240,
            cache_dir: PathBuf::from("/tmp/undertone-test"),
            cache_bytes: 1024 * 1024,
            mpv_binary: PathBuf::from("mpv"),
            transcode: false,
        }
    }

    fn app() -> App {
        let client = SubsonicClient::new(crate::subsonic::SubsonicConfig {
            server: "https://demo.navidrome.org".into(),
            username: "demo".into(),
            password: "demo".into(),
        });
        let art = ArtService::from_settings(
            ArtProtocol::Off,
            &PathBuf::from("/tmp/undertone-test-art"),
            1024,
            None,
        )
        .expect("art service");
        App::new(
            client,
            settings(),
            Box::new(NullPlayer::new()),
            art,
            ArtProtocol::Off,
            70,
        )
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn album(id: &str, songs: usize) -> Album {
        Album {
            id: id.to_string(),
            name: format!("album {id}"),
            song: (0..songs)
                .map(|i| Song {
                    id: format!("{id}-{i}"),
                    title: format!("track {i}"),
                    duration: Some(180),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn starts_on_the_albums_view() {
        assert_eq!(app().view, View::Albums);
    }

    #[test]
    fn j_and_k_move_the_selection_within_bounds() {
        let mut app = app();
        app.albums = vec![album("a", 1), album("b", 1), album("c", 1)];
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, 1);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
        // Clamped at the top.
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn g_and_big_g_jump_to_the_ends() {
        let mut app = app();
        app.albums = (0..10).map(|i| album(&i.to_string(), 1)).collect();
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.selected, 9);
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn q_quits_and_escape_goes_back() {
        let mut app = app();
        app.view = View::AlbumDetail;
        app.handle_key(key(KeyCode::Esc));
        assert_eq!(app.view, View::Albums);
        assert!(!app.should_quit);

        app.handle_key(key(KeyCode::Char('q')));
        assert!(app.should_quit);
    }

    #[test]
    fn help_toggles_back_to_albums() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(app.view, View::Help);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.view, View::Albums, "any key closes help");
    }

    #[test]
    fn playing_an_album_fills_the_queue_and_starts_playback() {
        let mut app = app();
        app.detail = Some(album("x", 3));
        app.play_album();

        assert_eq!(app.queue.len(), 3);
        assert_eq!(
            app.now.track.as_ref().map(|t| t.title.as_str()),
            Some("track 0")
        );
        assert!(!app.now.paused);
    }

    #[test]
    fn choosing_a_track_starts_the_queue_at_that_track() {
        let mut app = app();
        app.detail = Some(album("x", 3));
        app.play_track_from_album(2);
        assert_eq!(app.queue.position(), Some(2));
        assert_eq!(
            app.now.track.as_ref().map(|t| t.title.as_str()),
            Some("track 2")
        );
    }

    #[test]
    fn space_pauses_and_resumes() {
        let mut app = app();
        app.detail = Some(album("x", 1));
        app.play_album();
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(app.now.paused);
        app.handle_key(key(KeyCode::Char(' ')));
        assert!(!app.now.paused);
    }

    #[test]
    fn volume_is_clamped() {
        let mut app = app();
        app.volume = 98;
        app.change_volume(5);
        assert_eq!(app.volume, 100);
        app.volume = 2;
        app.change_volume(-5);
        assert_eq!(app.volume, 0);
    }

    #[test]
    fn repeat_and_shuffle_toggle() {
        let mut app = app();
        app.toggle_shuffle();
        assert!(app.queue.is_shuffled());
        app.cycle_repeat();
        assert_eq!(app.queue.repeat(), crate::queue::Repeat::All);
    }

    #[test]
    fn scrolling_keeps_the_selection_visible() {
        let mut app = app();
        app.albums = (0..50).map(|i| album(&i.to_string(), 1)).collect();
        app.selected = 30;
        // 24 rows of list area at 2 rows per entry = 12 visible entries.
        app.ensure_visible(24);
        assert!(app.scroll <= app.selected);
        assert!(app.selected < app.scroll + 12);

        app.selected = 0;
        app.ensure_visible(24);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn a_player_error_is_surfaced_not_fatal() {
        let mut app = app();
        app.error("something broke");
        let status = app.status.as_ref().expect("status");
        assert!(status.error);
        assert!(!app.should_quit);
    }
}
