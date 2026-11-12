//! Application state and input handling.
//!
//! Deliberately free of terminal I/O so it can be tested: keys go in, state
//! changes, and `ui::draw` renders it. The only blocking calls are explicit
//! user actions (opening an album, changing sort), never the tick path.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

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

/// How the album list is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AlbumLayout {
    #[default]
    List,
    /// A wall of covers. Cells are sized so the tile grid stays stable while
    /// scrolling, which keeps sixel repaints to whole rows.
    Grid,
}

/// Grid geometry, in terminal cells.
pub const GRID_TILE_W: u16 = 12;
pub const GRID_TILE_H: u16 = 5;
pub const GRID_THUMB_W: u16 = 10;
pub const GRID_THUMB_H: u16 = 4;

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
    pub layout: AlbumLayout,
    pub albums: Vec<Album>,
    pub sort: AlbumListType,
    /// The server returned a short page, so there is nothing after this.
    pub albums_exhausted: bool,
    pub selected: usize,
    pub scroll: usize,
    pub detail: Option<Album>,
    pub detail_selected: usize,

    pub now: NowPlaying,
    pub status: Option<Status>,
    pub should_quit: bool,
    pub needs_redraw: bool,
    /// Set when the screen must be cleared, not merely redrawn: erasing a
    /// sixel by blanking its cells does not remove the pixels.
    pub needs_clear: bool,
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
            layout: AlbumLayout::List,
            albums: Vec::new(),
            sort: AlbumListType::Random,
            // Nothing has been requested yet, so there is no next page to
            // fetch. `load_albums` clears this. Defaulting to `true` also
            // keeps tests that inject albums by hand from making network
            // calls when their cursor nears the end of the list.
            albums_exhausted: true,
            selected: 0,
            scroll: 0,
            detail: None,
            detail_selected: 0,
            now: NowPlaying::default(),
            status: None,
            should_quit: false,
            needs_redraw: true,
            needs_clear: false,
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

    /// Albums requested per page. Navidrome caps `getAlbumList2` at 500; a
    /// hundred keeps a page under about a second on a slow link while filling
    /// a screen several times over, so scrolling rarely waits.
    pub const ALBUM_PAGE: usize = 100;

    /// Albums fetched per scroll. Random order lands here.
    pub fn load_albums(&mut self) {
        self.albums.clear();
        self.albums_exhausted = false;
        self.selected = 0;
        self.scroll = 0;
        self.fetch_album_page();
        self.needs_redraw = true;
    }

    /// Append the next page. Called when the cursor nears the loaded end.
    pub fn fetch_album_page(&mut self) {
        if self.albums_exhausted {
            return;
        }
        let query = AlbumListQuery {
            kind: self.sort,
            size: Self::ALBUM_PAGE as u32,
            offset: self.albums.len() as u32,
            ..Default::default()
        };
        match self.client.album_list(&query) {
            Ok(page) => {
                let returned = page.len();
                self.albums.extend(page);
                // A short page is the last one. Some servers return exactly
                // `size` items and then an empty page; the next fetch handles
                // that without looping, since an empty page trips this too.
                if returned < Self::ALBUM_PAGE {
                    self.albums_exhausted = true;
                }
                self.needs_redraw = true;
            }
            Err(err) => {
                // Do not retry on every keystroke; the user can press R.
                self.albums_exhausted = true;
                self.error(format!("could not load more albums: {err}"));
            }
        }
    }

    /// Fetch ahead when the cursor is within a screen of the end.
    ///
    /// Blocking on purpose: one page is a single request, and the alternative
    /// is a background thread whose results have to be spliced into a list the
    /// cursor indexes. A brief hitch only when crossing a page boundary beats
    /// that complexity on hardware this slow.
    pub fn maybe_load_more(&mut self) {
        if self.albums_exhausted || self.albums.is_empty() {
            return;
        }
        let lookahead = 25;
        if self.selected + lookahead >= self.albums.len() {
            self.fetch_album_page();
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
                self.set_view(View::AlbumDetail);
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
                suffix: song.suffix.clone(),
                bit_rate: song.bit_rate,
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

    /// `p` / Space: pause what is playing, otherwise start the queue.
    ///
    /// Deliberately does not fall back to opening the selection: `p` means
    /// play, and silently loading an album because nothing was queued is the
    /// kind of surprise that makes a player feel unpredictable.
    pub fn play_or_pause(&mut self) {
        if self.now.track.is_some() {
            self.toggle_pause();
        } else if !self.queue.is_empty() {
            self.play_current();
        } else {
            self.status("nothing to play — Enter on an album first");
        }
    }

    /// Move the cursor by a screenful (Ctrl-d / Ctrl-u).
    fn page(&mut self, direction: i32) {
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        let rows = match (self.view, self.layout) {
            (View::Albums, AlbumLayout::Grid) => {
                let cols = self.grid_columns(self.list_area.0).max(1);
                (usize::from(self.list_area.1) / usize::from(GRID_TILE_H)).max(1) * cols
            }
            (View::Albums, AlbumLayout::List) => {
                (usize::from(self.list_area.1) / usize::from(ROW_HEIGHT)).max(1)
            }
            // Detail and queue rows are one line each; leave a row for the
            // view's own header so the page never overshoots the window.
            _ => (usize::from(self.list_area.1).saturating_sub(2)).max(1),
        };
        self.shift_selection(direction * rows as i32, len);
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

    /// Change view, forcing a screen clear when it actually changes.
    ///
    /// Blanking the cells that held a sixel leaves the pixels behind, so any
    /// movement between views has to clear the screen rather than diff it.
    pub fn set_view(&mut self, view: View) {
        if self.view != view {
            self.view = view;
            self.needs_clear = true;
            self.needs_redraw = true;
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) {
        self.needs_redraw = true;

        // Ctrl-d / Ctrl-u arrive as Char('d') / Char('u') with CONTROL (0x04
        // and 0x15). They are the only control keys the UI accepts, and they
        // have to be handled before the guard below.
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && !key.modifiers.contains(KeyModifiers::ALT)
        {
            match key.code {
                KeyCode::Char('d') => {
                    self.page(1);
                    return;
                }
                KeyCode::Char('u') => {
                    self.page(-1);
                    return;
                }
                _ => {}
            }
        }

        // A control byte decodes as a plain char plus a modifier: Ctrl+R is
        // 0x12, which arrives as Char('r') with CONTROL. Without this guard a
        // stray control byte (flow control, a stuck modifier, a terminal
        // feature) silently triggers plain-letter actions.
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return;
        }

        if self.view == View::Help {
            self.set_view(View::Albums);
            return;
        }

        match key.code {
            KeyCode::Char('q') if key.modifiers.is_empty() => self.should_quit = true,
            KeyCode::Esc | KeyCode::Char('h') if self.view != View::Albums => {
                self.set_view(View::Albums);
            }
            KeyCode::Char('?') => self.set_view(View::Help),
            KeyCode::Char('Q') => {
                let next = if self.view == View::Queue {
                    View::Albums
                } else {
                    View::Queue
                };
                self.set_view(next);
            }
            KeyCode::Char('w') => self.toggle_layout(),
            KeyCode::Char('h') if self.view == View::Albums => self.move_horizontal(-1),
            KeyCode::Char('l') if self.view == View::Albums => self.move_horizontal(1),
            KeyCode::Char('j') | KeyCode::Down => self.move_vertical(1),
            KeyCode::Char('k') | KeyCode::Up => self.move_vertical(-1),
            KeyCode::Char('g') => self.jump_cursor(0),
            KeyCode::Char('G') => self.jump_cursor(usize::MAX),
            KeyCode::Enter => self.activate(),
            KeyCode::Char(' ') | KeyCode::Char('p') => self.play_or_pause(),
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

    fn move_vertical(&mut self, delta: i32) {
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        // In the grid, one visual row is a whole line of tiles.
        let step = if self.view == View::Albums && self.layout == AlbumLayout::Grid {
            self.grid_columns(self.list_area.0).max(1) as i32
        } else {
            1
        };
        self.shift_selection(delta * step, len);
    }

    fn move_horizontal(&mut self, delta: i32) {
        if self.layout == AlbumLayout::List {
            return;
        }
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        self.shift_selection(delta, len);
    }

    fn shift_selection(&mut self, delta: i32, len: usize) {
        let next = (self.cursor() as i32 + delta).clamp(0, len as i32 - 1) as usize;
        match self.view {
            View::Albums => {
                self.selected = next;
                // Near the end of what we hold, pull the next page.
                self.maybe_load_more();
            }
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
            View::Albums => {
                self.selected = next;
                self.maybe_load_more();
            }
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
    ///
    /// In grid mode the window slides by whole rows, so tile positions stay
    /// fixed relative to the row and scrolling does not repaint everything.
    pub fn ensure_visible(&mut self, width: u16, height: u16) {
        match self.layout {
            AlbumLayout::List => {
                let visible = (usize::from(height) / usize::from(ROW_HEIGHT)).max(1);
                if self.selected < self.scroll {
                    self.scroll = self.selected;
                } else if self.selected >= self.scroll + visible {
                    self.scroll = self.selected + 1 - visible;
                }
            }
            AlbumLayout::Grid => {
                let cols = self.grid_columns(width).max(1);
                let visible_rows = (usize::from(height) / usize::from(GRID_TILE_H)).max(1);
                let selected_row = self.selected / cols;
                let first_row = self.scroll / cols;
                if selected_row < first_row {
                    self.scroll = selected_row * cols;
                } else if selected_row >= first_row + visible_rows {
                    self.scroll = (selected_row + 1 - visible_rows) * cols;
                }
            }
        }
    }

    /// Tiles per row for the current width.
    pub fn grid_columns(&self, width: u16) -> usize {
        (width / GRID_TILE_W).max(1) as usize
    }

    pub fn toggle_layout(&mut self) {
        self.layout = match self.layout {
            AlbumLayout::List => AlbumLayout::Grid,
            AlbumLayout::Grid => AlbumLayout::List,
        };
        self.scroll = 0;
        // Tiles are painted at different places in each layout, so the old
        // pixels must go: clear rather than diff.
        self.needs_clear = true;
        let label = match self.layout {
            AlbumLayout::List => "list",
            AlbumLayout::Grid => "grid",
        };
        self.status(format!("album view: {label}"));
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

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
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
    fn p_toggles_pause_when_something_is_playing() {
        let mut app = app();
        app.now.track = Some(crate::queue::QueueItem {
            id: "s1".into(),
            title: "a track".into(),
            ..Default::default()
        });
        app.now.paused = false;

        app.handle_key(key(KeyCode::Char('p')));
        assert!(app.now.paused, "p should pause a playing track");

        app.handle_key(key(KeyCode::Char('p')));
        assert!(!app.now.paused, "p should resume a paused track");
    }

    #[test]
    fn p_with_nothing_queued_says_so_instead_of_guessing() {
        let mut app = app();
        app.albums = vec![album("a", 2)];
        app.handle_key(key(KeyCode::Char('p')));
        assert!(app.queue.is_empty(), "p must not queue anything by itself");
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("nothing to play"), "{}", status.text);
    }

    #[test]
    fn ctrl_d_and_ctrl_u_page_by_a_screenful() {
        let mut app = app();
        app.albums = (0..60).map(|i| album(&i.to_string(), 1)).collect();
        // 20 rows at ROW_HEIGHT 2 is ten albums per page.
        app.list_area = (100, 20);

        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 10);

        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 20);

        app.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(app.selected, 10);

        // Clamped at the top rather than underflowing.
        app.handle_key(ctrl(KeyCode::Char('u')));
        app.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn ctrl_d_in_grid_pages_by_whole_rows() {
        let mut app = app();
        app.albums = (0..60).map(|i| album(&i.to_string(), 1)).collect();
        app.layout = AlbumLayout::Grid;
        // 96 wide is eight 12-cell tiles; 25 rows is five 5-row tile rows.
        app.list_area = (96, 25);

        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 40, "one page is five rows of eight");
    }

    #[test]
    fn paging_stays_put_with_no_items() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('d')));
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
        app.ensure_visible(100, 24);
        assert!(app.scroll <= app.selected);
        assert!(app.selected < app.scroll + 12);

        app.selected = 0;
        app.ensure_visible(100, 24);
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

    #[test]
    fn w_toggles_the_album_layout_and_says_so() {
        let mut app = app();
        assert_eq!(app.layout, AlbumLayout::List);
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(app.layout, AlbumLayout::Grid);
        assert_eq!(
            app.status.as_ref().map(|s| s.text.as_str()),
            Some("album view: grid")
        );
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(app.layout, AlbumLayout::List);
        assert_eq!(
            app.status.as_ref().map(|s| s.text.as_str()),
            Some("album view: list")
        );
    }

    #[test]
    fn grid_navigation_moves_by_rows_and_cells() {
        let mut app = app();
        app.layout = AlbumLayout::Grid;
        app.list_area = (100, 30);
        app.albums = (0..40).map(|i| album(&i.to_string(), 1)).collect();
        let cols = app.grid_columns(100);
        assert!(cols > 1);

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.selected, cols, "j moves one tile row");
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.selected, 0);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.selected, 1, "l moves one tile");
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn h_is_back_on_sub_views_but_moves_in_the_grid() {
        let mut app = app();
        app.albums = (0..10).map(|i| album(&i.to_string(), 1)).collect();
        app.list_area = (100, 30);

        // On the top-level view it moves the cursor, it does not quit or go back.
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.view, View::Albums);
        assert!(!app.should_quit);

        // Inside a sub-view it goes back.
        app.view = View::AlbumDetail;
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.view, View::Albums);
    }

    #[test]
    fn changing_view_requests_a_screen_clear() {
        let mut app = app();
        app.needs_clear = false;
        app.handle_key(key(KeyCode::Char('Q')));
        assert!(app.needs_clear, "view change must clear, not just redraw");

        app.needs_clear = false;
        app.handle_key(key(KeyCode::Char('Q')));
        assert!(app.needs_clear, "returning must clear too");
    }

    #[test]
    fn toggling_layout_requests_a_screen_clear() {
        let mut app = app();
        app.needs_clear = false;
        app.handle_key(key(KeyCode::Char('w')));
        assert!(
            app.needs_clear,
            "switching list to grid must clear the old thumbnails"
        );
    }

    #[test]
    fn staying_on_a_view_does_not_force_a_clear() {
        let mut app = app();
        app.albums = vec![album("a", 1)];
        app.needs_clear = false;
        app.handle_key(key(KeyCode::Char('j')));
        assert!(!app.needs_clear, "scrolling must not clear the screen");
    }

    #[test]
    fn control_bytes_do_not_trigger_plain_letter_actions() {
        let mut app = app();
        // Ctrl+R is byte 0x12 and decodes as Char('r') with CONTROL. It must
        // not cycle repeat, or a stray control byte looks like a phantom key.
        app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL));
        assert_eq!(app.queue.repeat(), crate::queue::Repeat::Off);

        for ch in ['s', 'm', 'w', 'q'] {
            app.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::CONTROL));
        }
        assert!(!app.should_quit, "Ctrl+Q must not quit");
        assert_eq!(app.layout, AlbumLayout::List);
        assert!(!app.queue.is_shuffled());
    }
}
