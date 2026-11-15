//! Application state and input handling.
//!
//! Deliberately free of terminal I/O so it can be tested: keys go in, state
//! changes, and `ui::draw` renders it. The only blocking calls are explicit
//! user actions (opening an album, changing sort), never the tick path.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::art::ArtService;
use crate::config::Settings;
use crate::listen_later::ListenLater;
use crate::player::{
    EndReason, LoadMode, Player, PlayerEvent, ScrobbleAction, Scrobbler, Seek, TrackSource,
};
use crate::queue::{Queue, QueueItem, Repeat, Step};
use crate::render::{ArtProtocol, CellSize, Painter, Placement};
use crate::session::Session;
use crate::subsonic::{
    Album, AlbumListQuery, AlbumListType, Artist, ArtistDetail, ArtistInfo, Playlist, Song,
    SubsonicClient,
};

/// Rows of text per album entry: title, subtitle, and the track count.
pub const ROW_HEIGHT: u16 = 3;

/// The wall's target tile, in cells.
///
/// A target, not a size: the wall fits as many whole tiles as it can by these,
/// then stretches every tile to fill the space it was given. Half a row of
/// covers is worse than three rows of slightly taller ones, and a blank band
/// at the bottom of the screen is worse than both.
pub const WALL_TILE_W: u16 = 14;
pub const WALL_TILE_H: u16 = 6;
/// Air between a cover and the next tile, and the line of text under it.
const TILE_GAP: u16 = 1;
const TILE_LABEL: u16 = 1;

/// One thumbnail on screen: the cells it occupies, and the box to render into.
///
/// The two are not the same size. chafa measures its output in cells of 10x20
/// pixels, but a terminal cell is whatever the font makes it - 16x41 on the
/// Pocket Reform. Images are drawn at their own pixel size, so asking for a
/// `6x3` chafa box put a 60x60 image inside a 96x123 slot: a cover using half
/// of the space reserved for it. Sizing the chafa box in real pixels is what
/// makes a thumbnail fill its tile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Thumb {
    /// Cells reserved on screen.
    pub cols: u16,
    pub rows: u16,
    /// `--size` for chafa, in chafa's own 10x20 cells.
    pub art_cols: u32,
    pub art_rows: u32,
}

/// The wall one area holds: how many tiles fit, and how big each one is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wall {
    pub cols: usize,
    pub rows: usize,
    pub thumb: Thumb,
    /// Tile pitch: a cover plus the air after it.
    pub tile_w: u16,
    pub tile_h: u16,
    /// Air on each side of a cover that is narrower than its tile, which is
    /// what keeps symbol art centred instead of shoved left.
    pad_x: u16,
    /// Cells left over once the tiles are placed, handed out between them so
    /// the wall ends where the window does.
    pub spare_w: u16,
    pub spare_h: u16,
}

impl Wall {
    /// Tiles on one screen.
    pub fn per_page(&self) -> usize {
        self.cols * self.rows
    }

    /// Where tile `slot` starts, as an offset from the area's top-left corner.
    /// The label under a cover runs the width of the tile from here.
    pub fn offset(&self, slot: usize) -> (u16, u16) {
        let cols = self.cols.max(1);
        let row = slot / cols;
        let col = slot % cols;
        let x = col as u16 * self.tile_w + spread(self.spare_w, self.cols, col);
        let y = row as u16 * self.tile_h + spread(self.spare_h, self.rows, row);
        (y, x)
    }

    /// Where the cover inside tile `slot` starts: the tile, plus the air that
    /// centres a cover narrower than the tile.
    pub fn cover_offset(&self, slot: usize) -> (u16, u16) {
        let (y, x) = self.offset(slot);
        (y, x + self.pad_x)
    }

    /// Room for the line of text under a cover.
    pub fn label_w(&self) -> u16 {
        self.tile_w.saturating_sub(TILE_GAP).max(1)
    }
}

/// Hand out `spare` leftover cells across `count` steps, so an area that does
/// not divide evenly still ends flush with the window.
fn spread(spare: u16, count: usize, step: usize) -> u16 {
    if count == 0 {
        return 0;
    }
    (u32::from(spare) * step as u32 / count as u32) as u16
}

/// Cells it takes to square off `cols` cells of art, given a cell in pixels.
fn rows_for(cols: u16, cell_w: u32, cell_h: u32) -> u16 {
    let px = u32::from(cols) * cell_w;
    ((px + cell_h / 2) / cell_h).max(1) as u16
}

/// Cells it takes to square off `rows` cells of art.
fn cols_for(rows: u16, cell_w: u32, cell_h: u32) -> u16 {
    let px = u32::from(rows) * cell_h;
    ((px + cell_w / 2) / cell_w).max(1) as u16
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Albums,
    AlbumDetail,
    Artists,
    ArtistDetail,
    Favorites,
    Queue,
    ListenLater,
    Playlists,
    PlaylistDetail,
    Search,
    Help,
}

impl AlbumLayout {
    pub fn key(self) -> &'static str {
        match self {
            AlbumLayout::List => "list",
            AlbumLayout::Grid => "grid",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        match key {
            "list" => Some(AlbumLayout::List),
            "grid" => Some(AlbumLayout::Grid),
            _ => None,
        }
    }
}

/// Which of the artist page's three panes has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtistPane {
    #[default]
    Albums,
    Similar,
    Biography,
}

impl ArtistPane {
    pub fn next(self) -> Self {
        match self {
            ArtistPane::Albums => ArtistPane::Similar,
            ArtistPane::Similar => ArtistPane::Biography,
            ArtistPane::Biography => ArtistPane::Albums,
        }
    }

    pub fn prev(self) -> Self {
        match self {
            ArtistPane::Albums => ArtistPane::Biography,
            ArtistPane::Similar => ArtistPane::Albums,
            ArtistPane::Biography => ArtistPane::Similar,
        }
    }
}

/// What the artist page's left pane is showing.
///
/// The page keeps three panes; this is a tab inside the first one, because
/// albums and top songs are the same question - what does this artist have -
/// and both are lists you open something from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtistLeft {
    #[default]
    Albums,
    Songs,
}

impl ArtistLeft {
    pub fn next(self) -> Self {
        match self {
            ArtistLeft::Albums => ArtistLeft::Songs,
            ArtistLeft::Songs => ArtistLeft::Albums,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ArtistLeft::Albums => "albums",
            ArtistLeft::Songs => "top songs",
        }
    }
}

/// One row of a search: albums open, songs play.
///
/// A result list is capped at seventy rows, so the width of the largest
/// variant costs tens of kilobytes at worst - not worth a `Box` at every use.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum SearchHit {
    Album(Album),
    Artist(Artist),
    Song(Song),
}

/// The search box and what it found.
#[derive(Debug, Default)]
pub struct SearchState {
    pub query: String,
    /// True while the query is being typed: keys type instead of acting.
    pub typing: bool,
    pub hits: Vec<SearchHit>,
    pub selected: usize,
    /// First result row on screen. Kept so moving back up walks the selection
    /// through the window instead of dragging the whole list with it.
    pub scroll: usize,
}

impl View {
    /// A stable name, for saving which list was open.
    pub fn key(self) -> &'static str {
        match self {
            View::Albums => "albums",
            View::AlbumDetail => "album",
            View::Artists => "artists",
            View::ArtistDetail => "artist",
            View::Favorites => "starred",
            View::ListenLater => "later",
            View::Playlists => "playlists",
            View::PlaylistDetail => "playlist",
            View::Search => "search",
            View::Queue => "queue",
            View::Help => "help",
        }
    }

    /// What to call this view in a message.
    pub fn name(self) -> &'static str {
        match self {
            View::Albums => "albums",
            View::AlbumDetail => "album",
            View::Artists => "artists",
            View::ArtistDetail => "artist",
            View::Favorites => "starred",
            View::ListenLater => "later",
            View::Playlists => "playlists",
            View::PlaylistDetail => "playlist",
            View::Search => "search",
            View::Queue => "queue",
            View::Help => "help",
        }
    }

    /// Only the lists that need no request on start come back.
    pub fn from_key(key: &str) -> Option<View> {
        match key {
            "albums" => Some(View::Albums),
            "artists" => Some(View::Artists),
            "starred" => Some(View::Favorites),
            "later" => Some(View::ListenLater),
            "playlists" => Some(View::Playlists),
            "queue" => Some(View::Queue),
            _ => None,
        }
    }
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

pub struct NowPlaying {
    pub track: Option<QueueItem>,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
    /// True once a stream has actually been handed to the player. A track
    /// restored from a previous session is shown, but nothing is loaded yet, so
    /// resuming has to start playback rather than unpause silence.
    pub loaded: bool,
    /// Queue playback index of playlist entry zero, and how many entries the
    /// player currently holds. Together they map mpv's playlist position back
    /// to a place in the queue without either side guessing.
    pub playlist_base: usize,
    pub playlist_len: usize,
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
            loaded: false,
            playlist_base: 0,
            playlist_len: 0,
            listened: 0.0,
            scrobbler: Scrobbler::default(),
        }
    }
}

/// How the artist list is ordered.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ArtistSort {
    /// Most albums first: in a library with no play data this is the best
    /// available proxy for "who matters here".
    #[default]
    MostAlbums,
    Name,
    /// Ranked by where their albums appear in the server's frequent list.
    MostPlayed,
}

impl ArtistSort {
    pub fn label(self) -> &'static str {
        match self {
            Self::MostAlbums => "most albums",
            Self::Name => "a-z",
            Self::MostPlayed => "most played",
        }
    }

    pub fn from_key(key: &str) -> Option<Self> {
        [Self::MostAlbums, Self::Name, Self::MostPlayed]
            .into_iter()
            .find(|sort| sort.label() == key)
    }

    fn next(self) -> Self {
        match self {
            Self::MostAlbums => Self::Name,
            Self::Name => Self::MostPlayed,
            Self::MostPlayed => Self::MostAlbums,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Status {
    pub text: String,
    pub at: Instant,
    pub error: bool,
}

/// Everything the artist page shows, kept when you leave it so `h` and `l`
/// can put it back without a request.
///
/// The page is the one screen whose state is overwritten by navigating within
/// it (artist → similar artist), so a bare `View` in the history cannot say
/// which artist you came from, let alone restore its biography.
#[derive(Debug, Clone)]
pub struct ArtistPage {
    detail: ArtistDetail,
    info: Option<ArtistInfo>,
    songs: Vec<Song>,
    pane: ArtistPane,
    left: ArtistLeft,
    album_selected: usize,
    album_scroll: usize,
    song_selected: usize,
    song_scroll: usize,
    similar_selected: usize,
    bio_scroll: usize,
}

/// One place in the navigation history.
#[derive(Debug, Clone)]
pub struct Location {
    view: View,
    /// Present only for an artist page.
    artist: Option<ArtistPage>,
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
    /// Where `h` goes. A stack rather than a rule, because an album can be
    /// opened from the library, favourites, an artist or listen later, and
    /// "back" has to mean the place you came from.
    pub nav_stack: Vec<Location>,
    /// Where `l` goes: the history of places you came back from, so `h` is not
    /// a one-way door.
    pub forward_stack: Vec<Location>,
    pub layout: AlbumLayout,
    pub artist_layout: AlbumLayout,
    pub favorites_layout: AlbumLayout,
    pub later_layout: AlbumLayout,
    pub albums: Vec<Album>,
    pub sort: AlbumListType,
    /// The server returned a short page, so there is nothing after this.
    pub albums_exhausted: bool,
    pub listen_later: ListenLater,
    /// `listen_later` rendered as albums, refreshed whenever the list changes.
    pub listen_albums: Vec<Album>,
    pub listen_selected: usize,
    pub listen_scroll: usize,

    /// Artists, flattened from the server's letter index.
    pub artists: Vec<Artist>,
    pub artist_selected: usize,
    pub artist_scroll: usize,
    pub artist_sort: ArtistSort,
    /// Artist id -> position in the server's most-played album list. Built once,
    /// on demand: it costs a request, so it only happens if you pick that sort.
    pub artist_play_rank: std::collections::HashMap<String, usize>,
    /// The open artist: albums, plus the biography and similar artists that
    /// come from a second, optional request.
    pub artist_detail: Option<ArtistDetail>,
    pub artist_info: Option<ArtistInfo>,
    pub artist_pane: ArtistPane,
    /// The artist's most played tracks, when the server knows them.
    pub artist_songs: Vec<Song>,
    pub artist_left: ArtistLeft,
    pub artist_song_selected: usize,
    pub artist_song_scroll: usize,
    pub artist_similar_selected: usize,
    pub artist_similar_scroll: usize,
    pub artist_bio_scroll: usize,
    /// The biography wrapped into its column: how many lines it makes and how
    /// many fit on screen. Filled while drawing, because only the renderer
    /// knows the pane's width.
    pub artist_bio_lines: usize,
    pub artist_bio_page: usize,
    /// The server's playlists, and the one being looked at.
    pub playlists: Vec<Playlist>,
    pub playlist_detail: Option<Playlist>,
    pub playlist_selected: usize,
    pub playlist_scroll: usize,
    pub playlist_detail_scroll: usize,
    pub search: SearchState,
    playlists_loaded: bool,
    pub artist_album_selected: usize,
    pub artist_album_scroll: usize,

    /// Starred albums.
    pub favorites: Vec<Album>,
    pub favorite_selected: usize,
    pub favorite_scroll: usize,
    /// The track Auto DJ last tried to extend from, so one barren track is not
    /// retried on every transition.
    pub auto_dj_seed: Option<String>,
    /// Where to jump once a restored track actually starts playing.
    pub pending_seek: Option<f64>,
    pub selected: usize,
    pub scroll: usize,
    pub detail: Option<Album>,
    pub detail_selected: usize,
    pub detail_scroll: usize,
    /// Where the queue cursor sits, as a playback-order row. `None` follows the
    /// track that is playing; moving the cursor pins it where you put it, so
    /// browsing the queue never changes what is playing or what plays next.
    pub queue_pin: Option<usize>,
    /// First queue row on screen, so walking back up the queue moves the
    /// selection through the window instead of pinning it to the bottom.
    pub queue_view_scroll: usize,

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
    /// Size of one terminal cell in pixels. Art is sized in real pixels, so
    /// this decides how much of a tile a cover fills.
    pub cell: CellSize,
}

impl App {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        client: SubsonicClient,
        settings: Settings,
        player: Box<dyn Player>,
        art: ArtService,
        protocol: ArtProtocol,
        cell: CellSize,
        volume: u8,
        listen_later: ListenLater,
    ) -> Self {
        let painter = Painter::new(protocol);
        let listen_albums = listen_later.albums();
        Self {
            client,
            queue: Queue::new(),
            player,
            art,
            painter,
            protocol,
            settings,
            view: View::Albums,
            nav_stack: Vec::new(),
            forward_stack: Vec::new(),
            layout: AlbumLayout::List,
            artist_layout: AlbumLayout::List,
            favorites_layout: AlbumLayout::List,
            later_layout: AlbumLayout::List,
            albums: Vec::new(),
            sort: AlbumListType::Random,
            // Nothing has been requested yet, so there is no next page to
            // fetch. `load_albums` clears this. Defaulting to `true` also
            // keeps tests that inject albums by hand from making network
            // calls when their cursor nears the end of the list.
            albums_exhausted: true,
            listen_later,
            listen_albums,
            listen_selected: 0,
            listen_scroll: 0,
            artists: Vec::new(),
            artist_selected: 0,
            artist_scroll: 0,
            artist_sort: ArtistSort::default(),
            artist_play_rank: std::collections::HashMap::new(),
            artist_detail: None,
            artist_info: None,
            artist_pane: ArtistPane::default(),
            artist_songs: Vec::new(),
            artist_left: ArtistLeft::default(),
            artist_song_selected: 0,
            artist_song_scroll: 0,
            artist_similar_selected: 0,
            artist_similar_scroll: 0,
            artist_bio_scroll: 0,
            artist_bio_lines: 0,
            artist_bio_page: 0,
            playlists: Vec::new(),
            playlist_detail: None,
            playlist_selected: 0,
            playlist_scroll: 0,
            playlist_detail_scroll: 0,
            search: SearchState::default(),
            playlists_loaded: false,
            artist_album_selected: 0,
            artist_album_scroll: 0,
            favorites: Vec::new(),
            favorite_selected: 0,
            favorite_scroll: 0,
            auto_dj_seed: None,
            pending_seek: None,
            selected: 0,
            scroll: 0,
            detail: None,
            detail_selected: 0,
            detail_scroll: 0,
            queue_pin: None,
            queue_view_scroll: 0,
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
            cell,
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

    /// Top songs asked for per artist. Twenty fills the pane and is what the
    /// server's own clients ask for; more would be a scroll of the same track.
    pub const TOP_SONGS: u32 = 20;

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

    /// `S`: cycle the sort of whatever list you are looking at.
    pub fn cycle_sort(&mut self) {
        match self.view {
            View::Albums | View::AlbumDetail => {
                let order = AlbumListType::ALL_IN_UI;
                let index = order
                    .iter()
                    .position(|kind| *kind == self.sort)
                    .unwrap_or(0);
                self.sort = order[(index + 1) % order.len()];
                let label = self.sort.label();
                self.load_albums();
                self.status(format!("albums: {label}"));
            }
            View::Artists | View::ArtistDetail => {
                self.artist_sort = self.artist_sort.next();
                self.sort_artists();
                let label = self.artist_sort.label();
                self.status(format!("artists: {label}"));
            }
            _ => self.status("nothing to sort here"),
        }
    }

    /// The label of the sort in force for the current view.
    pub fn sort_label(&self) -> &'static str {
        match self.view {
            View::Artists | View::ArtistDetail => self.artist_sort.label(),
            _ => self.sort.label(),
        }
    }

    /// Order the artist list in place.
    pub fn sort_artists(&mut self) {
        if self.artist_sort == ArtistSort::MostPlayed && self.artist_play_rank.is_empty() {
            self.load_artist_play_rank();
        }
        match self.artist_sort {
            ArtistSort::MostAlbums => self.artists.sort_by(|a, b| {
                b.album_count
                    .unwrap_or(0)
                    .cmp(&a.album_count.unwrap_or(0))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            }),
            ArtistSort::Name => self.artists.sort_by_key(|a| a.name.to_lowercase()),
            ArtistSort::MostPlayed => {
                let rank = &self.artist_play_rank;
                self.artists.sort_by(|a, b| {
                    rank.get(&a.id)
                        .copied()
                        .unwrap_or(usize::MAX)
                        .cmp(&rank.get(&b.id).copied().unwrap_or(usize::MAX))
                        .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
                });
            }
        }
        self.artist_selected = self
            .artist_selected
            .min(self.artists.len().saturating_sub(1));
        self.needs_redraw = true;
    }

    /// Rank artists by how early their albums appear in the frequent list.
    fn load_artist_play_rank(&mut self) {
        let query = AlbumListQuery {
            kind: AlbumListType::Frequent,
            size: 500,
            offset: 0,
            ..Default::default()
        };
        match self.client.album_list(&query) {
            Ok(albums) => {
                let mut rank: std::collections::HashMap<String, usize> =
                    std::collections::HashMap::new();
                for album in albums {
                    if let Some(artist_id) = album.artist_id {
                        let next = rank.len();
                        rank.entry(artist_id).or_insert(next);
                    }
                }
                self.artist_play_rank = rank;
            }
            Err(err) => self.error(format!("could not rank artists: {err}")),
        }
    }

    pub fn open_selected_album(&mut self) {
        let Some(album) = self.albums.get(self.selected).cloned() else {
            return;
        };
        self.open_album(album);
    }

    /// Fetch and show one album's detail view.
    fn open_album(&mut self, album: Album) {
        let id = album.id.clone();
        let name = album.name.clone();
        match self.client.album(&id) {
            Ok(album) => {
                self.detail = Some(album);
                self.detail_selected = 0;
                self.detail_scroll = 0;
                self.set_view(View::AlbumDetail);
            }
            Err(err) => {
                self.error(format!("could not open {name}: {err}"));
            }
        }
    }

    // -------------------------------------------------------- listen later

    /// `z`: add or remove the album under the cursor.
    ///
    /// On an album page that is the open album; on a search result it is the
    /// album hit; everywhere else it is the selected row of an album list. A
    /// song or a queue row has no album to save and says so.
    pub fn toggle_listen_later(&mut self) {
        let album = match self.view {
            View::Search => match self.search.hits.get(self.search.selected) {
                Some(SearchHit::Album(album)) => Some(album.clone()),
                _ => None,
            },
            View::ArtistDetail
                if self.artist_pane != ArtistPane::Albums
                    || self.artist_left != ArtistLeft::Albums =>
            {
                None
            }
            _ => self.album_under_cursor().map(|(album, _)| album),
        };
        let Some(album) = album else {
            self.status("no album under the cursor");
            return;
        };
        match self.listen_later.toggle(&album) {
            Ok(true) => self.status(format!("listen later: added {}", album.name)),
            Ok(false) => self.status(format!("listen later: removed {}", album.name)),
            Err(err) => self.error(format!("could not save listen later: {err}")),
        }
        self.refresh_listen_albums();
    }

    pub fn open_listen_later(&mut self) {
        self.refresh_listen_albums();
        self.jump_to_view(View::ListenLater);
        if self.listen_albums.is_empty() {
            self.status("listen later is empty - press z on an album to add it");
        }
    }

    fn refresh_listen_albums(&mut self) {
        self.listen_albums = self.listen_later.albums();
        if self.listen_selected >= self.listen_albums.len() {
            self.listen_selected = self.listen_albums.len().saturating_sub(1);
        }
    }

    // ---------------------------------------------------------- queue edits

    /// `a` / `A`: queue whatever is under the cursor.
    ///
    /// `after` tucks it in behind the playing track; otherwise it goes to the
    /// end. An album comes with all its tracks, which costs a request because
    /// the lists only carry counts - an explicit keypress is the right time to
    /// spend it.
    pub fn queue_selected(&mut self, at_end: bool) {
        if let Some(items) = self.song_items_under_cursor() {
            self.enqueue(items, at_end);
            return;
        }
        let album = match self.view {
            View::Search => match self.search.hits.get(self.search.selected) {
                Some(SearchHit::Album(album)) => Some(album.clone()),
                _ => None,
            },
            View::ArtistDetail
                if self.artist_pane != ArtistPane::Albums
                    || self.artist_left != ArtistLeft::Albums =>
            {
                None
            }
            _ => self.album_under_cursor().map(|(album, _)| album),
        };
        let Some(album) = album else {
            self.status("nothing under the cursor to queue");
            return;
        };
        match self.client.album(&album.id) {
            Ok(full) if !full.song.is_empty() => {
                let items = Self::to_queue_items(&full.song);
                self.enqueue(items, at_end);
            }
            Ok(_) => self.status(format!("{} has no tracks", album.name)),
            Err(err) => self.error(format!("could not queue {}: {err}", album.name)),
        }
    }

    /// The single song under the cursor, for a key that only makes sense on a
    /// track: album detail, an artist's top songs, a playlist, a search hit or
    /// a row of the queue itself.
    fn song_items_under_cursor(&self) -> Option<Vec<QueueItem>> {
        let item = match self.view {
            View::AlbumDetail => self
                .detail
                .as_ref()?
                .song
                .get(self.detail_selected)
                .map(queue_item),
            View::ArtistDetail
                if self.artist_pane == ArtistPane::Albums
                    && self.artist_left == ArtistLeft::Songs =>
            {
                self.artist_songs
                    .get(self.artist_song_selected)
                    .map(queue_item)
            }
            View::PlaylistDetail => self
                .playlist_detail
                .as_ref()?
                .entry
                .get(self.playlist_selected)
                .map(queue_item),
            View::Search => match self.search.hits.get(self.search.selected) {
                Some(SearchHit::Song(song)) => Some(queue_item(song)),
                _ => None,
            },
            View::Queue => self
                .queue
                .playback_items()
                .get(self.queue_cursor())
                .map(|item| (*item).clone()),
            _ => None,
        };
        item.map(|item| vec![item])
    }

    fn enqueue(&mut self, items: Vec<QueueItem>, at_end: bool) {
        let count = items.len();
        let what = if count == 1 {
            items[0].title.clone()
        } else {
            format!("{count} tracks")
        };
        if at_end {
            self.queue.append(items);
        } else {
            self.queue.insert_after_current(items);
            // The player already holds the old next tracks. Inserting between
            // them and what is playing only works if its window is rebuilt,
            // otherwise mpv walks straight past the track just queued.
            self.reload_player_window();
        }
        self.queue_view_scroll = 0;
        self.status(format!(
            "queued {what} {}",
            if at_end { "at the end" } else { "next" }
        ));
    }

    /// `o` in the queue: the album of the selected track.
    fn open_queue_album(&mut self) {
        let Some(item) = self.selected_queue_item() else {
            self.status("no track under the cursor");
            return;
        };
        let Some(id) = item.album_id.clone() else {
            self.status("this track names no album");
            return;
        };
        let album = Album {
            id,
            name: item.album.clone().unwrap_or_default(),
            ..Default::default()
        };
        self.open_album(album);
    }

    /// `i` in the queue: the artist of the selected track.
    fn open_queue_artist(&mut self) {
        let Some(item) = self.selected_queue_item() else {
            self.status("no track under the cursor");
            return;
        };
        let Some(id) = item.artist_id.clone() else {
            self.status("this track names no artist");
            return;
        };
        let name = item.artist.clone().unwrap_or_default();
        self.open_artist(&id, &name);
    }

    fn selected_queue_item(&self) -> Option<QueueItem> {
        self.queue
            .playback_items()
            .get(self.queue_cursor())
            .map(|item| (*item).clone())
    }

    /// `x`: drop the selected queue row.
    ///
    /// Removing the playing track starts whatever takes its place; removing an
    /// earlier or later one leaves the music alone. If the row was already in
    /// the player's lookahead, the window is rebuilt so mpv cannot play a
    /// track that is no longer in the queue.
    pub fn remove_queue_item(&mut self) {
        if self.view != View::Queue {
            return;
        }
        let index = self.queue_cursor();
        let current = self.queue.position();
        let base = self.now.playlist_base;
        // Any removal inside the window invalidates mpv's position mapping,
        // including a row it has already played; only a row before the window
        // can be accounted for by shifting the base.
        let in_window = self.now.loaded && index >= base && index < base + self.now.playlist_len;
        let Some(removed) = self.queue.remove(index) else {
            return;
        };
        if self.queue.is_empty() {
            let _ = self.player.stop();
            self.now = NowPlaying::default();
            self.pending_seek = None;
            self.queue_pin = None;
            self.status(format!("removed {}", removed.title));
            return;
        }
        if Some(index) == current {
            // The row that was playing is gone; the queue's new current takes
            // over. A restored session has not started playing yet, so it is
            // only shown, not started.
            if self.now.loaded {
                self.play_current();
            } else {
                self.adopt_current_silently();
            }
            self.status(format!("removed {}", removed.title));
            return;
        }
        if let Some(pin) = self.queue_pin {
            // The rows after the removed one move up by one; a pinned cursor
            // moves with them rather than jumping to the hole.
            let pin = if index < pin { pin - 1 } else { pin };
            self.queue_pin = Some(pin.min(self.queue.len().saturating_sub(1)));
        }
        if index < base {
            // The window starts later now: its first entry is one queue row
            // closer to the front.
            self.now.playlist_base = base - 1;
        } else if in_window {
            self.reload_player_window();
        }
        self.status(format!("removed {}", removed.title));
    }

    /// `J` / `K`: move the selected queue row down / up.
    ///
    /// The cursor follows the row you are carrying, and the player window is
    /// rebuilt when something is playing because mpv holds its own copy of the
    /// order.
    pub fn move_queue_item(&mut self, delta: i32) {
        if self.view != View::Queue || self.queue.len() < 2 {
            return;
        }
        let from = self.queue_cursor();
        let to = (from as i32 + delta).clamp(0, self.queue.len() as i32 - 1) as usize;
        if from == to || !self.queue.move_item(from, to) {
            return;
        }
        if self.queue_pin.is_some() {
            self.queue_pin = Some(to);
        }
        self.reload_player_window();
    }

    // ---------------------------------------------------------------- artists

    /// Run the query. Deliberately on Enter, not on every keystroke: the client
    /// is synchronous and each request would freeze the interface mid-word.
    pub fn run_search(&mut self) {
        self.search.typing = false;
        let query = self.search.query.trim().to_string();
        if query.is_empty() {
            self.status("type a query first");
            return;
        }

        match self.client.search(&query) {
            Ok(result) => {
                self.search.hits = result
                    .album
                    .into_iter()
                    .map(SearchHit::Album)
                    .chain(result.artist.into_iter().map(SearchHit::Artist))
                    .chain(result.song.into_iter().map(SearchHit::Song))
                    .collect();
                self.search.selected = 0;
                self.search.scroll = 0;
                if self.view != View::Search {
                    self.set_view(View::Search);
                }
                let found = self.search.hits.len();
                if found == 0 {
                    self.status(format!("nothing found for “{query}”"));
                } else {
                    self.status(format!("{found} results for “{query}”"));
                }
            }
            Err(err) => self.error(format!("search failed: {err}")),
        }
    }

    /// The server's playlists. Fetched once: the list changes only when someone
    /// edits it, and `R` re-reads it like every other list.
    pub fn open_playlists(&mut self) {
        if !self.playlists_loaded {
            match self.client.playlists() {
                Ok(playlists) => self.playlists = playlists,
                Err(err) => {
                    self.error(format!("could not load playlists: {err}"));
                    return;
                }
            }
            self.playlists_loaded = true;
        }
        self.jump_to_view(View::Playlists);
        self.playlist_selected = 0;
        self.playlist_scroll = 0;
        if self.playlists.is_empty() {
            self.status("no playlists on this server");
        }
    }

    /// One playlist, with its entries.
    pub fn open_playlist(&mut self, id: &str) {
        match self.client.playlist(id) {
            Ok(playlist) => {
                self.playlist_detail = Some(playlist);
                self.playlist_selected = 0;
                self.playlist_detail_scroll = 0;
                self.set_view(View::PlaylistDetail);
            }
            Err(err) => self.error(format!("could not load playlist: {err}")),
        }
    }

    /// Put the whole playlist in the queue and start at `index`.
    pub fn play_playlist_track(&mut self, index: usize) {
        let Some(playlist) = self.playlist_detail.clone() else {
            return;
        };
        if playlist.entry.is_empty() {
            self.status("this playlist is empty");
            return;
        }
        let index = index.min(playlist.entry.len() - 1);
        let items: Vec<QueueItem> = playlist.entry.iter().map(queue_item).collect();
        self.queue.replace(items);
        self.queue.jump_to(index);
        self.play_current();
    }

    /// The artist list (`2`), fetched once and kept.
    pub fn open_artists(&mut self) {
        if self.artists.is_empty() {
            match self.client.artists() {
                Ok(index) => {
                    // The server groups artists by initial; flatten it, because
                    // a flat list is what a keyboard can traverse.
                    self.artists = index
                        .index
                        .into_iter()
                        .flat_map(|group| group.artist)
                        .collect();
                }
                Err(err) => {
                    self.error(format!("could not load artists: {err}"));
                    return;
                }
            }
        }
        self.jump_to_view(View::Artists);
        self.sort_artists();
        if self.artists.is_empty() {
            self.status("no artists on this server");
        }
    }

    pub fn open_selected_artist(&mut self) {
        let Some(artist) = self.artists.get(self.artist_selected).cloned() else {
            return;
        };
        self.open_artist(&artist.id, &artist.name);
    }

    /// Open an artist by id, from wherever it was named.
    ///
    /// The albums come from `getArtist` and are required; the biography and
    /// similar artists come from `getArtistInfo2` and are not. A server without
    /// that endpoint (or without Last.fm data) still gets a usable page.
    pub fn open_artist(&mut self, id: &str, name: &str) {
        // Following a similar artist from an artist page overwrites that page,
        // so remember where we are *before* the new data lands. `set_view`
        // cannot do it: by then the page it would save is already the new one.
        if self.artist_id() == Some(id) {
            return;
        }
        let here = self.location();
        match self.client.artist(id) {
            Ok(detail) => {
                self.nav_stack.push(here);
                self.forward_stack.clear();
                self.artist_detail = Some(detail);
                self.artist_album_selected = 0;
                self.artist_album_scroll = 0;
                self.artist_pane = ArtistPane::Albums;
                self.artist_similar_selected = 0;
                self.artist_similar_scroll = 0;
                self.artist_bio_scroll = 0;
                self.artist_left = ArtistLeft::Albums;
                self.artist_song_selected = 0;
                self.artist_song_scroll = 0;
                self.artist_info = self.client.artist_info(id).ok();
                // Top songs are keyed by artist *name*, and are optional: a
                // server without listening history returns nothing.
                self.artist_songs = self
                    .client
                    .top_songs(name, Self::TOP_SONGS)
                    .unwrap_or_default();
                self.view = View::ArtistDetail;
                self.needs_clear = true;
                self.needs_redraw = true;
            }
            Err(err) => self.error(format!("could not open {name}: {err}")),
        }
    }

    /// Play a top song, with the rest of the artist's top songs behind it.
    pub fn play_artist_song(&mut self, index: usize) {
        if self.artist_songs.is_empty() {
            self.status("no top songs for this artist");
            return;
        }
        let index = index.min(self.artist_songs.len() - 1);
        let items: Vec<QueueItem> = self.artist_songs.iter().map(queue_item).collect();
        self.queue.replace(items);
        self.queue.jump_to(index);
        self.play_current();
    }

    /// `i` on an album: the artist behind it.
    pub fn open_album_artist(&mut self) {
        let Some(album) = self.detail.as_ref() else {
            return;
        };
        let Some(id) = album.artist_id.clone() else {
            self.status("this album names no artist");
            return;
        };
        let name = album.artist.clone().unwrap_or_default();
        self.open_artist(&id, &name);
    }

    // ------------------------------------------------------------- favourites

    /// Starred albums (`3`).
    pub fn open_favorites(&mut self) {
        match self.client.starred() {
            Ok(starred) => {
                self.favorites = starred.album;
                self.favorite_selected = self
                    .favorite_selected
                    .min(self.favorites.len().saturating_sub(1));
                self.jump_to_view(View::Favorites);
                if self.favorites.is_empty() {
                    self.status("nothing starred yet - press * on an album");
                }
            }
            Err(err) => self.error(format!("could not load favourites: {err}")),
        }
    }

    /// The album the cursor is on, and whether it is currently starred.
    fn album_under_cursor(&self) -> Option<(Album, bool)> {
        let album = match self.view {
            View::Albums => self.albums.get(self.selected),
            View::ListenLater => self.listen_albums.get(self.listen_selected),
            View::Favorites => self.favorites.get(self.favorite_selected),
            View::ArtistDetail => self
                .artist_detail
                .as_ref()
                .and_then(|detail| detail.album.get(self.artist_album_selected)),
            View::AlbumDetail => self.detail.as_ref(),
            _ => None,
        }?;
        Some((album.clone(), album.starred.is_some()))
    }

    /// `*`: star or unstar the album under the cursor.
    pub fn toggle_star(&mut self) {
        let Some((album, starred)) = self.album_under_cursor() else {
            self.status("no album under the cursor");
            return;
        };
        let result = if starred {
            self.client.unstar(&album.id)
        } else {
            self.client.star(&album.id)
        };
        match result {
            Ok(()) => {
                // Reflect it locally rather than refetching: the server has
                // accepted the change, and a round trip here would make the
                // key feel laggy on a slow link. No status line either: the
                // star appearing in the row is the feedback, and a message
                // saying what the row already says is noise on the bar.
                self.apply_star(&album.id, !starred);
            }
            Err(err) => self.error(format!("could not update star: {err}")),
        }
    }

    fn apply_star(&mut self, album_id: &str, starred: bool) {
        let stamp = if starred {
            Some(String::from("now"))
        } else {
            None
        };
        let targets = self
            .albums
            .iter_mut()
            .chain(self.listen_albums.iter_mut())
            .chain(self.favorites.iter_mut())
            .chain(self.detail.iter_mut())
            .chain(
                self.artist_detail
                    .iter_mut()
                    .flat_map(|detail| detail.album.iter_mut()),
            );
        for album in targets {
            if album.id == album_id {
                album.starred = stamp.clone();
            }
        }
        // The saved copy is what the listen later page redraws from, so it has
        // to learn about the star too - otherwise the star vanishes the next
        // time that page is opened.
        if let Err(err) = self.listen_later.set_starred(album_id, stamp) {
            self.error(format!("could not save listen later: {err}"));
        }
        if !starred {
            self.favorites.retain(|album| album.id != album_id);
            if self.favorite_selected >= self.favorites.len() {
                self.favorite_selected = self.favorites.len().saturating_sub(1);
            }
        }
    }

    // ------------------------------------------------------------ playback

    fn stream_url(&self, song_id: &str) -> String {
        self.client.stream_url(song_id)
    }

    fn to_queue_items(songs: &[Song]) -> Vec<QueueItem> {
        songs.iter().map(queue_item).collect()
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

    /// Where the queue cursor sits: the row you moved to, or the track that is
    /// playing when you have not moved it.
    pub fn queue_cursor(&self) -> usize {
        match self.queue.len() {
            0 => 0,
            len => self
                .queue_pin
                .or_else(|| self.queue.position())
                .unwrap_or(0)
                .min(len - 1),
        }
    }

    pub fn play_current(&mut self) {
        let Some(_) = self.queue.current() else {
            return;
        };
        let base = self.queue.position().unwrap_or(0);
        let window: Vec<TrackSource> = self
            .queue
            .window(PLAYLIST_WINDOW)
            .iter()
            .map(|item| TrackSource {
                url: self.stream_url(&item.id),
                title: item.title.clone(),
            })
            .collect();
        if window.is_empty() {
            return;
        }

        if let Err(err) = self.player.load(&window, LoadMode::Replace) {
            self.error(format!("player: {err}"));
            return;
        }
        let _ = self.player.play();
        self.now.playlist_base = base;
        self.now.playlist_len = window.len();
        // A track now plays, so the queue cursor goes back to following it:
        // the row you were browsing was a question about what to play, and it
        // has been answered.
        self.queue_pin = None;
        self.adopt_current();

        // A restored session resumes where it left off, once the stream is
        // actually open.
        if let Some(seconds) = self.pending_seek.take() {
            if self.player.seek(Seek::Absolute, seconds).is_ok() {
                self.now.position = seconds;
            }
        }
    }

    /// Rebuild the player's lookahead window around the track that is playing.
    ///
    /// Used when the queue changes under the player - inserting a track "next"
    /// is the case that matters. The window is replaced, so the stream restarts
    /// and is seeked back to where it was; state that belongs to the play
    /// (scrobble progress, listened time) is left alone.
    fn reload_player_window(&mut self) {
        if !self.now.loaded {
            return;
        }
        let Some(_) = self.queue.current() else {
            return;
        };
        let base = self.queue.position().unwrap_or(0);
        let window: Vec<TrackSource> = self
            .queue
            .window(PLAYLIST_WINDOW)
            .iter()
            .map(|item| TrackSource {
                url: self.stream_url(&item.id),
                title: item.title.clone(),
            })
            .collect();
        if window.is_empty() {
            return;
        }
        let seconds = self.now.position;
        let paused = self.now.paused;
        if self.player.load(&window, LoadMode::Replace).is_err() {
            return;
        }
        self.now.playlist_base = base;
        self.now.playlist_len = window.len();
        if seconds > 0.5 {
            let _ = self.player.seek(Seek::Absolute, seconds);
        }
        if paused {
            let _ = self.player.pause(true);
        }
    }

    /// Show the queue's current track in the bar without starting it.
    ///
    /// This is the restored-session state: a track is named and selected, but
    /// nothing has been handed to the player until you press play.
    fn adopt_current_silently(&mut self) {
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        self.now.track = Some(item.clone());
        self.now.position = 0.0;
        self.now.duration = f64::from(item.duration.unwrap_or(0));
        self.now.paused = true;
        self.now.loaded = false;
        self.pending_seek = None;
        self.needs_redraw = true;
    }

    /// Take the queue's current track as what is playing, without touching the
    /// player. Used when opening a window and when mpv moves to the next entry
    /// on its own.
    fn adopt_current(&mut self) {
        let Some(item) = self.queue.current().cloned() else {
            return;
        };
        self.now.track = Some(item.clone());
        self.now.position = 0.0;
        self.now.duration = f64::from(item.duration.unwrap_or(0));
        self.now.listened = 0.0;
        self.now.paused = false;
        self.now.loaded = true;
        self.needs_redraw = true;

        if self.settings.scrobble {
            let actions = self.now.scrobbler.start(&item.id, unix_millis());
            self.dispatch_scrobbles(actions);
        }

        // Top the queue up from here rather than from the tick: this runs once
        // per track, so a slow server costs one transition instead of making
        // the UI stutter every second.
        self.auto_dj_top_up();
    }

    /// Follow mpv when it moves to another playlist entry.
    ///
    /// This is what makes playback gapless: the player advances on its own and
    /// the app keeps its state in step, instead of stopping and reloading
    /// between tracks.
    fn follow_playlist(&mut self, index: usize) {
        let target = self.now.playlist_base + index;
        if self.queue.position() != Some(target) {
            if self.queue.jump_to(target).is_none() {
                return;
            }
            self.adopt_current();
        }
        self.top_up_playlist(index);
    }

    /// Keep at least two entries ahead in the player, so the next transition
    /// needs no network round trip.
    fn top_up_playlist(&mut self, index: usize) {
        if index + 2 < self.now.playlist_len {
            return;
        }
        let already = self.now.playlist_len;
        let extra: Vec<TrackSource> = self
            .queue
            .playback_items()
            .iter()
            .skip(self.now.playlist_base + already)
            .take(PLAYLIST_WINDOW)
            .map(|item| TrackSource {
                url: self.stream_url(&item.id),
                title: item.title.clone(),
            })
            .collect();
        if extra.is_empty() {
            return;
        }
        let count = extra.len();
        if self.player.load(&extra, LoadMode::Append).is_ok() {
            self.now.playlist_len += count;
        }
    }

    /// The player ran out of entries.
    fn on_playlist_finished(&mut self) {
        match self.queue.advance() {
            Step::Advanced | Step::Stayed => self.play_current(),
            Step::Finished => {
                self.now.paused = true;
                self.now.loaded = false;
                self.status("end of queue");
            }
        }
    }

    // ------------------------------------------------------------------ auto dj

    /// How few tracks may remain before the queue is topped up.
    /// Tracks handed to the player at once. Two ahead of what is playing is
    /// enough for the next transition to be gapless, and cheap in memory: an
    /// entry is a URL and a title.
    pub const AUTO_DJ_MIN_AHEAD: usize = 2;
    /// How many similar tracks one top-up asks the server for.
    pub const AUTO_DJ_BATCH: u32 = 10;

    /// Extend the queue with similar tracks when it is running out.
    ///
    /// Blocking, and deliberately so: it runs once per track transition, and
    /// the alternative - a background fetch - would have to splice songs into a
    /// queue the cursor indexes while the user is moving through it.
    pub fn auto_dj_top_up(&mut self) {
        if !self.settings.auto_dj {
            return;
        }
        // Repeat means the queue does not run out by itself, so there is
        // nothing to top up. Appending anyway would grow the queue without
        // bound and interleave new tracks into an order the user chose.
        if self.queue.repeat() != Repeat::Off {
            return;
        }
        let Some(seed) = self.now.track.as_ref().map(|track| track.id.clone()) else {
            return;
        };
        if self.queue.upcoming(Self::AUTO_DJ_MIN_AHEAD).len() >= Self::AUTO_DJ_MIN_AHEAD {
            return;
        }
        // One attempt per track. A server that knows nothing similar must not
        // be asked again every time the same track comes round.
        if self.auto_dj_seed.as_deref() == Some(seed.as_str()) {
            return;
        }
        self.auto_dj_seed = Some(seed.clone());

        let similar = self
            .client
            .similar_songs(&seed, Self::AUTO_DJ_BATCH)
            .unwrap_or_default();
        let mut additions = self.unheard(similar);
        if additions.is_empty() {
            // `getSimilarSongs` needs Last.fm data or a rich library: on the
            // demo it returns tracks from the same album, which are usually
            // already queued. Falling back only on *error* would leave Auto DJ
            // doing nothing at all on such a server, so fall back when the
            // similar result adds nothing new either. Keeping the music
            // playing beats stopping dead.
            additions = self.unheard(
                self.client
                    .random_songs(Self::AUTO_DJ_BATCH)
                    .unwrap_or_default(),
            );
        }

        if additions.is_empty() {
            return;
        }
        let count = additions.len();
        let items: Vec<QueueItem> = additions.iter().map(queue_item).collect();
        self.queue.append(items);
        self.status(format!("auto dj: {count} more queued"));
        self.needs_redraw = true;
    }

    /// Drop candidates already queued, and the track that seeded them.
    fn unheard(&self, candidates: Vec<Song>) -> Vec<Song> {
        let queued: std::collections::HashSet<&str> = self
            .queue
            .items()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        candidates
            .into_iter()
            .filter(|song| !queued.contains(song.id.as_str()))
            .take(Self::AUTO_DJ_BATCH as usize)
            .collect()
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
        // Nothing loaded yet (a restored session, or a fresh start): there is
        // no stream to unpause, so start the current track instead.
        if self.now.track.is_none() || !self.now.loaded {
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
            self.status("nothing to play - Enter on an album first");
        }
    }

    /// Move the cursor by a screenful (Ctrl-d / Ctrl-u).
    fn page(&mut self, direction: i32) {
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        let (width, height) = self.list_area;
        let rows = match self.layout_of(self.view) {
            Some(AlbumLayout::Grid) => self.wall(width, height).per_page(),
            Some(AlbumLayout::List) => list_rows(height),
            None if self.view == View::ArtistDetail
                && self.artist_pane == ArtistPane::Biography =>
            {
                self.artist_bio_page.max(1)
            }
            // Detail and queue rows are one line each; leave a row for the
            // view's own header so the page never overshoots the window.
            None => (usize::from(height).saturating_sub(2)).max(1),
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
            self.on_player_event(event);
        }

        if self.art.poll() {
            self.needs_redraw = true;
        }
    }

    /// One player event, applied to the app's state.
    ///
    /// Separate from `pump` so the dispatch itself can be tested without a
    /// player that can be poked.
    pub fn on_player_event(&mut self, event: PlayerEvent) {
        match event {
            PlayerEvent::Position(position) => {
                self.now.position = position;
                self.needs_redraw = true;
            }
            PlayerEvent::Duration(duration) => {
                self.now.duration = duration;
                self.needs_redraw = true;
            }
            // A player that has just started reports its own initial state
            // (`pause=no`) before anything is loaded. Believing it would
            // un-pause a restored track that is meant to sit still until you
            // ask for sound.
            PlayerEvent::Paused(paused) if self.now.loaded => {
                self.now.paused = paused;
                self.needs_redraw = true;
            }
            PlayerEvent::Paused(_) => {}
            // mpv moved on by itself: this is the gapless transition.
            PlayerEvent::PlaylistPos(index) => self.follow_playlist(index),
            // With a window loaded, mpv reports the end of *each* entry and then
            // moves on by itself; the playlist position is the signal that the
            // track changed, and `Idle` means it ran out. Advancing on `Ended`
            // as well would skip a track.
            PlayerEvent::Ended(reason) => {
                // `Stop` is what the player reports when we replace its
                // playlist, so only a real failure is worth a message.
                if matches!(reason, EndReason::Error | EndReason::Redirect) {
                    self.error("track failed to play - skipping to the next");
                }
            }
            PlayerEvent::Idle if !self.now.loaded => {
                // mpv is spawned idle and says so before anything is loaded.
                // That idle is not the end of a queue: acting on it would
                // start playing on launch, which is exactly what a restored
                // session promises not to do.
            }
            PlayerEvent::Idle => self.on_playlist_finished(),
            PlayerEvent::Crashed(message) => {
                // The player is gone, so nothing is loaded any more: `p` must
                // restart the track rather than pause a corpse.
                self.now.loaded = false;
                self.now.paused = true;
                self.error(format!("player crashed: {message}"));
            }
            PlayerEvent::FileLoaded | PlayerEvent::Volume(_) => {}
        }
    }

    /// Per-second accounting: progress and scrobble thresholds.
    pub fn tick(&mut self) {
        let elapsed = self.last_tick.elapsed().as_secs_f64();
        self.last_tick = Instant::now();
        if elapsed <= 0.0 {
            return;
        }

        let playing = self.now.loaded && self.now.track.is_some() && !self.now.paused;
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
            self.nav_stack.push(self.location());
            // Going somewhere new is not "forward": the way ahead is a dead end
            // once you pick another door. Help is a modal, so it leaves the way
            // ahead exactly where it found it.
            if view != View::Help {
                self.forward_stack.clear();
            }
            self.view = view;
            self.needs_clear = true;
            self.needs_redraw = true;
        }
    }

    /// The screen as it stands, ready to be put back.
    fn location(&self) -> Location {
        Location {
            view: self.view,
            artist: if self.view == View::ArtistDetail {
                self.artist_detail.as_ref().map(|detail| ArtistPage {
                    detail: detail.clone(),
                    info: self.artist_info.clone(),
                    songs: self.artist_songs.clone(),
                    pane: self.artist_pane,
                    left: self.artist_left,
                    album_selected: self.artist_album_selected,
                    album_scroll: self.artist_album_scroll,
                    song_selected: self.artist_song_selected,
                    song_scroll: self.artist_song_scroll,
                    similar_selected: self.artist_similar_selected,
                    bio_scroll: self.artist_bio_scroll,
                })
            } else {
                None
            },
        }
    }

    /// Put a history entry back on screen.
    fn restore(&mut self, target: Location) {
        if let Some(page) = target.artist {
            self.artist_detail = Some(page.detail);
            self.artist_info = page.info;
            self.artist_songs = page.songs;
            self.artist_pane = page.pane;
            self.artist_left = page.left;
            self.artist_album_selected = page.album_selected;
            self.artist_album_scroll = page.album_scroll;
            self.artist_song_selected = page.song_selected;
            self.artist_song_scroll = page.song_scroll;
            self.artist_similar_selected = page.similar_selected;
            self.artist_bio_scroll = page.bio_scroll;
        }
        self.view = target.view;
        self.needs_clear = true;
        self.needs_redraw = true;
    }

    /// The artist page currently on screen, if any.
    fn artist_id(&self) -> Option<&str> {
        if self.view != View::ArtistDetail {
            return None;
        }
        self.artist_detail.as_ref().map(|artist| artist.id.as_str())
    }

    /// Move to a top-level view from the rail: the back stack resets, because
    /// the rail is navigation, not a drill-down.
    pub fn jump_to_view(&mut self, view: View) {
        if view == View::Queue {
            // Opening the queue shows where you are, not where you last looked.
            self.queue_pin = None;
            self.queue_view_scroll = 0;
        }
        if self.view != view {
            self.view = view;
            self.needs_clear = true;
            self.needs_redraw = true;
        }
        self.nav_stack.clear();
        self.forward_stack.clear();
    }

    /// `h` / Esc: return to wherever this view was opened from.
    ///
    /// Always clears: coming back re-renders a different list at different
    /// offsets, and a plain diff leaves fragments of the previous screen -
    /// long titles in particular - behind.
    pub fn go_back(&mut self) {
        // No history (a restored session, say) falls back to the library: a key
        // that does nothing reads as broken.
        let target = self.nav_stack.pop().unwrap_or(Location {
            view: View::Albums,
            artist: None,
        });
        let already_there = target.view == self.view
            && match &target.artist {
                Some(page) => self.artist_id() == Some(page.detail.id.as_str()),
                None => self.view != View::ArtistDetail,
            };
        if already_there {
            return;
        }
        // Help is a modal, not a place: leaving it must not become somewhere
        // `l` can take you back to.
        if self.view != View::Help {
            self.forward_stack.push(self.location());
        }
        self.restore(target);
    }

    /// `l`: back to where you were before you pressed `h`.
    pub fn go_forward(&mut self) {
        let Some(target) = self.forward_stack.pop() else {
            return;
        };
        if target.view == View::Help {
            return;
        }
        self.nav_stack.push(self.location());
        self.restore(target);
    }

    /// The layout a view is using, if it has one.
    pub fn layout_of(&self, view: View) -> Option<AlbumLayout> {
        match view {
            View::Albums => Some(self.layout),
            View::Artists => Some(self.artist_layout),
            View::Favorites => Some(self.favorites_layout),
            View::ListenLater => Some(self.later_layout),
            _ => None,
        }
    }

    fn set_layout(&mut self, view: View, layout: AlbumLayout) {
        match view {
            View::Albums => self.layout = layout,
            View::Artists => {
                self.artist_layout = layout;
                self.artist_scroll = 0;
            }
            View::Favorites => {
                self.favorites_layout = layout;
                self.favorite_scroll = 0;
            }
            View::ListenLater => {
                self.later_layout = layout;
                self.listen_scroll = 0;
            }
            _ => {}
        }
    }

    /// `w`: rows or a wall, for whichever list you are looking at.
    pub fn toggle_current_layout(&mut self) {
        let Some(current) = self.layout_of(self.view) else {
            self.status("this view has no wall");
            return;
        };
        let next = match current {
            AlbumLayout::List => AlbumLayout::Grid,
            AlbumLayout::Grid => AlbumLayout::List,
        };
        self.set_layout(self.view, next);
        // Tiles are painted at different places in each layout, so the old
        // pixels must go: clear rather than diff.
        self.needs_clear = true;
        let label = match next {
            AlbumLayout::List => "list",
            AlbumLayout::Grid => "wall",
        };
        self.status(format!("{}: {label}", self.view.name()));
    }

    /// A thumbnail wall owns h/j/k/l: on the wall they move between covers, so
    /// history needs no key there.
    pub fn in_wall(&self) -> bool {
        self.layout_of(self.view) == Some(AlbumLayout::Grid)
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

        // While a query is being typed every key is text, not a command.
        if self.search.typing {
            match key.code {
                KeyCode::Enter => self.run_search(),
                KeyCode::Esc => self.search.typing = false,
                KeyCode::Backspace => {
                    self.search.query.pop();
                }
                KeyCode::Char(c) => {
                    self.search.query.push(c);
                }
                _ => {}
            }
            return;
        }

        if self.view == View::Help {
            self.go_back();
            return;
        }

        match key.code {
            KeyCode::Char('q') if key.modifiers.is_empty() => self.should_quit = true,
            KeyCode::Char('/') => {
                self.search.typing = true;
                self.search.query.clear();
            }
            // On a wall, h/l belong to the wall; everywhere else they walk the
            // history, which makes `h` reversible.
            KeyCode::Char('h') if !self.in_wall() => self.go_back(),
            KeyCode::Char('l') if !self.in_wall() => self.go_forward(),
            KeyCode::Esc => self.go_back(),
            KeyCode::Char('?') => self.set_view(View::Help),
            KeyCode::Char('Q') => {
                let next = if self.view == View::Queue {
                    View::Albums
                } else {
                    View::Queue
                };
                self.set_view(next);
            }
            KeyCode::Char('w') => self.toggle_current_layout(),
            // Queue management, on albums and songs alike: `a` tucks the
            // selection in after what is playing, `A` puts it at the end.
            KeyCode::Char('a') => self.queue_selected(false),
            KeyCode::Char('A') => self.queue_selected(true),
            // `z` for "zzz": the old `a` (save for later) moved aside.
            KeyCode::Char('z') => self.toggle_listen_later(),
            KeyCode::Char('o') if self.view == View::Queue => self.open_queue_album(),
            KeyCode::Char('i') if self.view == View::Queue => self.open_queue_artist(),
            KeyCode::Char('x') if self.view == View::Queue => self.remove_queue_item(),
            KeyCode::Char('J') if self.view == View::Queue => self.move_queue_item(1),
            KeyCode::Char('K') if self.view == View::Queue => self.move_queue_item(-1),
            KeyCode::Char('i') if self.view == View::AlbumDetail => self.open_album_artist(),
            KeyCode::Tab if self.view == View::ArtistDetail => {
                self.artist_pane = self.artist_pane.next()
            }
            KeyCode::BackTab if self.view == View::ArtistDetail => {
                self.artist_pane = self.artist_pane.prev()
            }
            KeyCode::Char('t') if self.view == View::ArtistDetail => {
                self.artist_left = self.artist_left.next();
                self.artist_pane = ArtistPane::Albums;
                self.status(format!("artist: {}", self.artist_left.label()));
            }
            // The rail, with explicit jumps.
            KeyCode::Char('1') => self.jump_to_view(View::Albums),
            KeyCode::Char('2') => self.open_artists(),
            KeyCode::Char('3') => self.open_favorites(),
            KeyCode::Char('4') => self.open_listen_later(),
            KeyCode::Char('5') => self.open_playlists(),
            KeyCode::Char('6') => self.jump_to_view(View::Queue),
            KeyCode::Char('*') => self.toggle_star(),
            KeyCode::Char('h') if self.in_wall() => self.move_horizontal(-1),
            KeyCode::Char('l') if self.in_wall() => self.move_horizontal(1),
            KeyCode::Left if !self.in_wall() => self.go_back(),
            KeyCode::Right if !self.in_wall() => self.go_forward(),
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
            KeyCode::Char('R') => {
                // Re-read whichever list is on screen, and forget the playlist
                // latch so `5` fetches again after an edit elsewhere.
                self.playlists_loaded = false;
                match self.view {
                    View::Playlists => self.open_playlists(),
                    _ => self.load_albums(),
                }
            }
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
            View::Artists => self.artists.len(),
            View::ArtistDetail => match self.artist_pane {
                ArtistPane::Albums => match self.artist_left {
                    ArtistLeft::Albums => self
                        .artist_detail
                        .as_ref()
                        .map(|artist| artist.album.len())
                        .unwrap_or(0),
                    ArtistLeft::Songs => self.artist_songs.len(),
                },
                ArtistPane::Similar => self
                    .artist_info
                    .as_ref()
                    .map(|info| info.similar_artist.len())
                    .unwrap_or(0),
                // The biography is scrolled, not selected. Its wrapped
                // line count comes from the renderer, which is the only part
                // that knows how wide the pane is.
                ArtistPane::Biography => self.artist_bio_lines,
            },
            View::Favorites => self.favorites.len(),
            View::Queue => self.queue.len(),
            View::ListenLater => self.listen_albums.len(),
            View::Search => self.search.hits.len(),
            View::Playlists => self.playlists.len(),
            View::PlaylistDetail => self
                .playlist_detail
                .as_ref()
                .map(|playlist| playlist.entry.len())
                .unwrap_or(0),
            View::Help => 0,
        }
    }

    fn cursor(&self) -> usize {
        match self.view {
            View::Albums => self.selected,
            View::AlbumDetail => self.detail_selected,
            View::Artists => self.artist_selected,
            View::ArtistDetail => match self.artist_pane {
                ArtistPane::Albums => match self.artist_left {
                    ArtistLeft::Albums => self.artist_album_selected,
                    ArtistLeft::Songs => self.artist_song_selected,
                },
                ArtistPane::Similar => self.artist_similar_selected,
                ArtistPane::Biography => self.artist_bio_scroll,
            },
            View::Favorites => self.favorite_selected,
            View::Queue => self.queue_cursor(),
            View::ListenLater => self.listen_selected,
            View::Playlists => self.playlist_selected,
            View::PlaylistDetail => self.playlist_selected,
            View::Search => self.search.selected,
            View::Help => 0,
        }
    }

    fn move_vertical(&mut self, delta: i32) {
        let len = self.cursor_len();
        if len == 0 {
            return;
        }
        // In the grid, one visual row is a whole line of tiles.
        let grid = self.layout_of(self.view).unwrap_or(AlbumLayout::List);
        let (width, height) = self.list_area;
        let step = if grid == AlbumLayout::Grid {
            self.wall(width, height).cols.max(1) as i32
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
            View::Artists => self.artist_selected = next,
            View::ArtistDetail => match self.artist_pane {
                ArtistPane::Albums => match self.artist_left {
                    ArtistLeft::Albums => self.artist_album_selected = next,
                    ArtistLeft::Songs => self.artist_song_selected = next,
                },
                ArtistPane::Similar => self.artist_similar_selected = next,
                ArtistPane::Biography => {
                    let max = self.bio_max_scroll();
                    self.artist_bio_scroll = next.min(max);
                }
            },
            View::Favorites => self.favorite_selected = next,
            View::Queue => self.queue_pin = Some(next),
            View::ListenLater => self.listen_selected = next,
            View::Playlists | View::PlaylistDetail => self.playlist_selected = next,
            View::Search => self.search.selected = next,
            View::Help => {}
        }
    }

    /// How far down the biography can go while a full pane stays on screen.
    fn bio_max_scroll(&self) -> usize {
        self.artist_bio_lines
            .saturating_sub(self.artist_bio_page.max(1))
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
            View::Artists => self.artist_selected = next,
            View::ArtistDetail => match self.artist_pane {
                ArtistPane::Albums => self.artist_album_selected = next,
                ArtistPane::Similar => self.artist_similar_selected = next,
                ArtistPane::Biography => self.artist_bio_scroll = target.min(self.bio_max_scroll()),
            },
            View::Favorites => self.favorite_selected = next,
            View::Queue => self.queue_pin = Some(next),
            View::ListenLater => self.listen_selected = next,
            View::Playlists | View::PlaylistDetail => self.playlist_selected = next,
            View::Search => self.search.selected = next,
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
                // The track list is indexed by the cursor directly: the cover
                // sits in the header above it, not in the list. Enter plays
                // from the selected track, and the whole album is queued
                // behind it either way.
                self.play_track_from_album(self.detail_selected);
            }
            View::Queue => {
                // Enter plays from the cursor, which is not the same row as the
                // one playing: selecting in the queue must not change playback.
                if self.queue.jump_to(self.queue_cursor()).is_some() {
                    self.play_current();
                }
            }
            View::ListenLater => {
                if let Some(album) = self.listen_albums.get(self.listen_selected).cloned() {
                    self.open_album(album);
                }
            }
            View::Artists => self.open_selected_artist(),
            View::Playlists => {
                if let Some(playlist) = self.playlists.get(self.playlist_selected).cloned() {
                    self.open_playlist(&playlist.id);
                }
            }
            View::PlaylistDetail => self.play_playlist_track(self.playlist_selected),
            View::Search => match self.search.hits.get(self.search.selected).cloned() {
                Some(SearchHit::Album(album)) => self.open_album(album),
                Some(SearchHit::Artist(artist)) => self.open_artist(&artist.id, &artist.name),
                Some(SearchHit::Song(song)) => {
                    self.queue.replace(vec![queue_item(&song)]);
                    self.queue.jump_to(0);
                    self.play_current();
                }
                None => {}
            },
            View::ArtistDetail => match self.artist_pane {
                ArtistPane::Albums if self.artist_left == ArtistLeft::Songs => {
                    self.play_artist_song(self.artist_song_selected)
                }
                ArtistPane::Albums => {
                    let album = self
                        .artist_detail
                        .as_ref()
                        .and_then(|artist| artist.album.get(self.artist_album_selected))
                        .cloned();
                    if let Some(album) = album {
                        self.open_album(album);
                    }
                }
                ArtistPane::Similar => {
                    let similar = self
                        .artist_info
                        .as_ref()
                        .and_then(|info| info.similar_artist.get(self.artist_similar_selected))
                        .cloned();
                    if let Some(artist) = similar {
                        self.open_artist(&artist.id, &artist.name);
                    }
                }
                // A biography has nothing to open.
                ArtistPane::Biography => {}
            },
            View::Favorites => {
                if let Some(album) = self.favorites.get(self.favorite_selected).cloned() {
                    self.open_album(album);
                }
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
                let wall = self.wall(width, height);
                scroll_grid(&mut self.selected, &mut self.scroll, wall.cols, wall.rows)
            }
        }
    }

    /// Keep the listen-later cursor inside its window.
    pub fn ensure_listen_visible(&mut self, width: u16, height: u16) {
        match self.later_layout {
            AlbumLayout::List => scroll_into_view(
                &mut self.listen_selected,
                &mut self.listen_scroll,
                list_rows(height),
            ),
            AlbumLayout::Grid => {
                let wall = self.wall(width, height);
                scroll_grid(
                    &mut self.listen_selected,
                    &mut self.listen_scroll,
                    wall.cols,
                    wall.rows,
                )
            }
        }
    }

    /// Keep the favourites cursor inside its window.
    pub fn ensure_favorite_visible(&mut self, width: u16, height: u16) {
        match self.favorites_layout {
            AlbumLayout::List => scroll_into_view(
                &mut self.favorite_selected,
                &mut self.favorite_scroll,
                list_rows(height),
            ),
            AlbumLayout::Grid => {
                let wall = self.wall(width, height);
                scroll_grid(
                    &mut self.favorite_selected,
                    &mut self.favorite_scroll,
                    wall.cols,
                    wall.rows,
                )
            }
        }
    }

    /// Keep the artist-list cursor inside its window.
    pub fn ensure_artist_visible(&mut self, width: u16, height: u16) {
        match self.artist_layout {
            AlbumLayout::List => scroll_into_view(
                &mut self.artist_selected,
                &mut self.artist_scroll,
                list_rows(height),
            ),
            AlbumLayout::Grid => {
                let wall = self.wall(width, height);
                scroll_grid(
                    &mut self.artist_selected,
                    &mut self.artist_scroll,
                    wall.cols,
                    wall.rows,
                )
            }
        }
    }

    /// Keep a one-line-per-row cursor inside its window. These views used to
    /// pin the selection to the bottom row, which dragged the whole list down
    /// (and back up) under the cursor.
    pub fn ensure_detail_track_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.detail_selected,
            &mut self.detail_scroll,
            usize::from(height).max(1),
        );
    }

    pub fn ensure_queue_visible(&mut self, height: u16) {
        let mut selected = self.queue_cursor();
        let mut scroll = self.queue_view_scroll;
        scroll_into_view(&mut selected, &mut scroll, usize::from(height).max(1));
        self.queue_view_scroll = scroll;
    }

    pub fn ensure_playlist_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.playlist_selected,
            &mut self.playlist_scroll,
            usize::from(height).max(1),
        );
    }

    pub fn ensure_playlist_detail_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.playlist_selected,
            &mut self.playlist_detail_scroll,
            usize::from(height).max(1),
        );
    }

    pub fn ensure_artist_songs_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.artist_song_selected,
            &mut self.artist_song_scroll,
            usize::from(height).max(1),
        );
    }

    pub fn ensure_artist_similar_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.artist_similar_selected,
            &mut self.artist_similar_scroll,
            usize::from(height).max(1),
        );
    }

    pub fn ensure_search_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.search.selected,
            &mut self.search.scroll,
            usize::from(height).max(1),
        );
    }

    /// Keep the artist-albums cursor inside its window.
    pub fn ensure_artist_album_visible(&mut self, height: u16) {
        scroll_into_view(
            &mut self.artist_album_selected,
            &mut self.artist_album_scroll,
            list_rows(height),
        );
    }

    // ---------------------------------------------------------------- session

    /// Restore the queue from a previous run.
    ///
    /// Deliberately silent: the track is shown in the bar and selected in the
    /// queue, but nothing is streamed until you press play. An app that starts
    /// making noise because it remembered something is a bad housemate.
    pub fn restore_session(&mut self, session: &Session) {
        if !session.queue.is_empty() {
            let position = session.position.min(session.queue.len().saturating_sub(1));
            self.queue.replace(session.queue.clone());
            self.queue.jump_to(position);
            if let Some(volume) = session.volume {
                self.volume = volume;
                let _ = self.player.set_volume(volume);
            }
            if let Some(item) = self.queue.current().cloned() {
                self.now.duration = f64::from(item.duration.unwrap_or(0));
                self.now.track = Some(item);
                self.now.position = session.seconds.max(0.0);
                self.now.paused = true;
                self.now.loaded = false;
                // Applied when playback actually starts: seeking a stream that
                // is not open does nothing.
                if session.seconds > 1.0 {
                    self.pending_seek = Some(session.seconds);
                }
            }
            self.status("restored last session - p to play");
        }

        // Sort and layout come back, because they are how you left the place
        // rather than something you would think to set again after every start.
        if let Some(sort) = session.sort.as_deref().and_then(AlbumListType::from_key) {
            self.sort = sort;
        }
        if let Some(sort) = session
            .artist_sort
            .as_deref()
            .and_then(ArtistSort::from_key)
        {
            self.artist_sort = sort;
        }
        if let Some(layout) = session
            .artist_layout
            .as_deref()
            .and_then(AlbumLayout::from_key)
        {
            self.artist_layout = layout;
        }
        if let Some(layout) = session.layout.as_deref().and_then(AlbumLayout::from_key) {
            self.layout = layout;
        }
        if let Some(layout) = session
            .starred_layout
            .as_deref()
            .and_then(AlbumLayout::from_key)
        {
            self.favorites_layout = layout;
        }
        if let Some(layout) = session
            .later_layout
            .as_deref()
            .and_then(AlbumLayout::from_key)
        {
            self.later_layout = layout;
        }

        // Back to the list you were looking at, when getting there costs no
        // request. Everything else opens on the library.
        match session.view.as_deref().and_then(View::from_key) {
            Some(View::Queue) => self.jump_to_view(View::Queue),
            Some(View::ListenLater) => self.open_listen_later(),
            _ => {}
        }
        self.needs_redraw = true;
    }

    /// What to save on the way out.
    pub fn session_snapshot(&self) -> Session {
        Session {
            version: crate::session::VERSION,
            queue: self.queue.items().to_vec(),
            position: self.queue.position().unwrap_or(0),
            volume: Some(self.volume),
            // Only a playing track has a position worth keeping; a restored,
            // never-started one sits at zero by definition.
            seconds: if self.now.loaded {
                self.now.position
            } else {
                0.0
            },
            sort: Some(self.sort.as_str().to_string()),
            artist_sort: Some(self.artist_sort.label().to_string()),
            artist_layout: Some(self.artist_layout.key().to_string()),
            layout: Some(self.layout.key().to_string()),
            starred_layout: Some(self.favorites_layout.key().to_string()),
            later_layout: Some(self.later_layout.key().to_string()),
            view: Some(self.view.key().to_string()),
        }
    }

    /// The size of one cell in the units art is measured in.
    ///
    /// Pixel protocols draw an image at its own pixel size, so that unit is
    /// the terminal's real cell. Symbol art is characters, laid out by chafa
    /// in its own 10x20 cell, so that is what keeps it square.
    fn art_cell(&self) -> (u32, u32) {
        if self.protocol.is_pixel() {
            (self.cell.width.max(1), self.cell.height.max(1))
        } else {
            (10, 20)
        }
    }

    /// The wall this area can hold.
    pub fn wall(&self, width: u16, height: u16) -> Wall {
        let cols = usize::from((width / WALL_TILE_W).max(1));
        let rows = usize::from((height / WALL_TILE_H).max(1));
        // Divide first, then let the tiles grow into the remainder: whole rows
        // of covers that fill the screen beat fewer rows and a blank band.
        let tile_w = (width / cols as u16).max(1);
        let tile_h = (height / rows as u16).max(TILE_LABEL + 1);
        let usable_w = tile_w.saturating_sub(TILE_GAP).max(1);
        let usable_h = tile_h.saturating_sub(TILE_LABEL).max(1);
        let thumb = self.square_thumb(usable_w, usable_h);
        Wall {
            cols,
            rows,
            thumb,
            tile_w,
            tile_h,
            pad_x: usable_w.saturating_sub(thumb.cols) / 2,
            spare_w: width.saturating_sub(cols as u16 * tile_w),
            spare_h: height.saturating_sub(rows as u16 * tile_h),
        }
    }

    /// The thumbnail for one list row: as tall as the row, and square.
    pub fn list_thumb(&self) -> Thumb {
        let (cell_w, cell_h) = self.art_cell();
        self.square_thumb(cols_for(ROW_HEIGHT, cell_w, cell_h), ROW_HEIGHT)
    }

    /// The largest square cover that fits in this many cells.
    ///
    /// Width is tried first, because a wall of covers reads across, and the
    /// height follows to keep the cover square; if that overflows, the height
    /// wins and the width follows instead.
    pub fn square_thumb(&self, avail_w: u16, avail_h: u16) -> Thumb {
        let (cell_w, cell_h) = self.art_cell();
        let avail_w = avail_w.max(1);
        let avail_h = avail_h.max(1);
        let mut cols = avail_w;
        let mut rows = rows_for(cols, cell_w, cell_h);
        if rows > avail_h {
            rows = avail_h;
            cols = cols_for(rows, cell_w, cell_h);
        }
        // chafa's box is counted in whole 20-pixel steps, and a square box is
        // what a square cover fills exactly - no letterbox, no dead band.
        let px = (u32::from(cols) * cell_w)
            .min(u32::from(rows) * cell_h)
            .max(20)
            / 20
            * 20;
        Thumb {
            cols,
            rows,
            art_cols: px / 10,
            art_rows: px / 20,
        }
    }
}

/// How many tracks to hand the player at once.
///
/// mpv runs from one entry to the next without reopening the audio device, so
/// this is what removes the gap between tracks. Three keeps two ahead of the
/// playing one, which is enough lookahead for a slow stream to start.
pub const PLAYLIST_WINDOW: usize = 3;

/// How many list rows fit in `height`, at three lines per row.
fn list_rows(height: u16) -> usize {
    (usize::from(height) / usize::from(ROW_HEIGHT)).max(1)
}

/// Slide a window so `selected` is visible.
fn scroll_into_view(selected: &mut usize, scroll: &mut usize, visible: usize) {
    if *selected < *scroll {
        *scroll = *selected;
    } else if *selected >= *scroll + visible {
        *scroll = *selected + 1 - visible;
    }
}

/// Keep a cursor inside a grid window, in whole rows so tiles do not shift.
fn scroll_grid(selected: &mut usize, scroll: &mut usize, cols: usize, visible_rows: usize) {
    let cols = cols.max(1);
    let visible_rows = visible_rows.max(1);
    let selected_row = *selected / cols;
    let first_row = *scroll / cols;
    if selected_row < first_row {
        *scroll = selected_row * cols;
    } else if selected_row >= first_row + visible_rows {
        *scroll = (selected_row + 1 - visible_rows) * cols;
    }
}

/// One song as the queue and the now-playing bar need it.
fn queue_item(song: &Song) -> QueueItem {
    QueueItem {
        id: song.id.clone(),
        title: song.title.clone(),
        artist: song.artist.clone(),
        album: song.album.clone(),
        album_id: song.album_id.clone(),
        artist_id: song.artist_id.clone(),
        cover_art: song.cover_art.clone(),
        duration: song.duration,
        suffix: song.suffix.clone(),
        bit_rate: song.bit_rate,
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
            palette: crate::config::Palette::Partial,
            cache_dir: PathBuf::from("/tmp/undertone-test"),
            cache_bytes: 1024 * 1024,
            mpv_binary: PathBuf::from("mpv"),
            backdrop: [0, 0, 0],
        }
    }

    fn app() -> App {
        let client = SubsonicClient::new(crate::subsonic::SubsonicConfig {
            server: "https://demo.navidrome.org".into(),
            username: "demo".into(),
            password: "demo".into(),
        });
        let art = ArtService::from_settings(
            ArtProtocol::Symbols,
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
            ArtProtocol::Symbols,
            CellSize {
                width: 8,
                height: 17,
            },
            70,
            fresh_listen_later(),
        )
    }

    /// A listen-later list unique to one test, so parallel tests cannot see
    /// each other's saves.
    fn fresh_listen_later() -> ListenLater {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "undertone-ll-test-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        ListenLater::load(path)
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
        // Playing implies a stream is loaded and the queue holds the track.
        app.now.loaded = true;
        app.now.paused = false;
        app.queue.replace(vec![track("s1")]);

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
        // 20 rows at ROW_HEIGHT 3 is six albums per page.
        app.list_area = (100, 20);

        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 6);

        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 12);

        app.handle_key(ctrl(KeyCode::Char('u')));
        assert_eq!(app.selected, 6);

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
        // 96 wide fits six 14-cell tiles, and 25 rows fits four six-row ones:
        // the wall divides the height by the target tile and lets the tiles
        // grow into the rest, so a page is a whole number of rows.
        app.list_area = (96, 25);

        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 24, "one page is four rows of six");
    }

    /// The geometry the device actually runs: a 16x41 cell, as the Pocket
    /// Reform's font reports it.
    fn pocket_reform() -> App {
        let mut app = app();
        app.protocol = ArtProtocol::Sixel;
        app.cell = CellSize {
            width: 16,
            height: 41,
        };
        app
    }

    #[test]
    fn a_wall_spends_the_height_it_is_given() {
        let app = pocket_reform();
        // The body of a 115x25 terminal: 101 columns by 19 rows.
        let wall = app.wall(101, 19);

        assert_eq!((wall.cols, wall.rows), (7, 3), "three rows of seven");
        assert_eq!(
            wall.rows as u16 * wall.tile_h + wall.spare_h,
            19,
            "no band of empty rows below the wall"
        );
        let (last_row, _) = wall.offset(wall.per_page() - 1);
        assert!(
            last_row + wall.tile_h <= 19,
            "every tile is fully inside the area"
        );
    }

    #[test]
    fn a_cover_fills_its_cell_slot() {
        let app = pocket_reform();
        let thumb = app.wall(101, 19).thumb;

        // 200x200 px of art in a 13x5 cell slot, which is 208x205 px.
        assert_eq!((thumb.art_cols, thumb.art_rows), (20, 10));
        assert_eq!(
            thumb.art_cols * 10,
            thumb.art_rows * 20,
            "a square box, so the cover is not letterboxed"
        );
        assert!(
            thumb.art_cols * 10 <= u32::from(thumb.cols) * 16,
            "and it fits"
        );
        assert!(thumb.art_rows * 20 <= u32::from(thumb.rows) * 41);
        assert!(
            thumb.art_cols * 10 * 100 / (u32::from(thumb.cols) * 16) >= 90,
            "the cover uses its slot instead of floating in it"
        );
    }

    #[test]
    fn a_list_thumbnail_is_as_tall_as_its_row() {
        let app = pocket_reform();
        let thumb = app.list_thumb();

        assert_eq!((thumb.art_cols, thumb.art_rows), (12, 6), "120x120 px");
        assert!(thumb.rows <= ROW_HEIGHT);
        assert!(thumb.art_rows * 20 <= u32::from(thumb.rows) * 41);
        assert!(
            thumb.cols + 1 < 20,
            "the text still gets the width of the row"
        );
    }

    #[test]
    fn symbol_art_keeps_two_of_width_to_one_of_height() {
        // Symbols are characters, laid out in chafa's own 10x20 cell, so a
        // square cover is twice as wide as it is tall.
        let wall = app().wall(101, 19);
        assert_eq!(wall.thumb.art_cols, wall.thumb.art_rows * 2);
    }

    #[test]
    fn search_scrolling_holds_still_while_the_cursor_walks_the_window() {
        let mut app = app();
        app.search.hits = (0..20)
            .map(|i| SearchHit::Song(song(&i.to_string())))
            .collect();

        app.search.selected = 4;
        app.ensure_search_visible(5);
        assert_eq!(app.search.scroll, 0, "the first screen stays put");

        app.search.selected = 7;
        app.ensure_search_visible(5);
        assert_eq!(app.search.scroll, 3, "a full window scrolls just enough");

        app.search.selected = 6;
        app.ensure_search_visible(5);
        assert_eq!(app.search.scroll, 3, "moving up walks inside the window");

        app.search.selected = 2;
        app.ensure_search_visible(5);
        assert_eq!(app.search.scroll, 2, "only the top edge pulls the list");
    }

    #[test]
    fn paging_stays_put_with_no_items() {
        let mut app = app();
        app.handle_key(ctrl(KeyCode::Char('d')));
        assert_eq!(app.selected, 0);
    }

    #[test]
    fn z_saves_the_selected_album_and_4_lists_it() {
        let mut app = app();
        app.albums = vec![album("a", 1), album("b", 1)];

        app.handle_key(key(KeyCode::Char('z')));
        assert_eq!(app.listen_albums.len(), 1);
        assert_eq!(app.listen_albums[0].id, "a");

        app.handle_key(key(KeyCode::Char('4')));
        assert_eq!(app.view, View::ListenLater);

        // Pressing z on the row removes it again.
        app.handle_key(key(KeyCode::Char('z')));
        assert!(app.listen_albums.is_empty());
    }

    #[test]
    fn listen_later_round_trips_through_the_file() {
        let mut app = app();
        app.albums = vec![album("a", 1)];
        app.handle_key(key(KeyCode::Char('z')));

        // A fresh load of the same path sees the entry.
        let path = app.listen_later.path().to_path_buf();
        assert!(ListenLater::load(&path).contains("a"));
    }

    #[test]
    fn vim_keys_move_within_listen_later() {
        let mut app = app();
        app.albums = vec![album("a", 1), album("b", 1), album("c", 1)];
        // Move between toggles: `z` on the same row would add then remove.
        app.handle_key(key(KeyCode::Char('z')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('z')));
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('z')));
        // Newest first: c, b, a.
        app.handle_key(key(KeyCode::Char('4')));
        assert_eq!(app.listen_selected, 0);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.listen_selected, 1);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.listen_selected, 2);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.listen_selected, 1);
    }

    // -------------------------------------------------------------- auto dj

    fn track(id: &str) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            title: format!("track {id}"),
            ..Default::default()
        }
    }

    fn song(id: &str) -> Song {
        Song {
            id: id.to_string(),
            title: format!("song {id}"),
            ..Default::default()
        }
    }

    #[test]
    fn auto_dj_does_nothing_when_disabled() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.now.track = Some(track("seed"));
        app.now.loaded = true;
        app.queue.replace(vec![track("seed")]);
        app.auto_dj_top_up();
        assert_eq!(app.queue.len(), 1, "disabled auto dj must not extend");
        assert!(app.auto_dj_seed.is_none());
    }

    #[test]
    fn auto_dj_leaves_a_healthy_queue_alone_without_asking_the_server() {
        let mut app = app();
        app.settings.auto_dj = true;
        app.now.track = Some(track("seed"));
        app.now.loaded = true;
        app.queue
            .replace(vec![track("seed"), track("a"), track("b"), track("c")]);
        app.auto_dj_top_up();
        // Enough ahead that no request is made at all: the seed is only
        // recorded when we actually try.
        assert!(app.auto_dj_seed.is_none());
        assert_eq!(app.queue.len(), 4);
    }

    #[test]
    fn unheard_drops_what_is_already_queued() {
        let mut app = app();
        app.queue.replace(vec![track("a"), track("b")]);
        let picked = app.unheard(vec![song("b"), song("c"), song("c")]);
        let ids: Vec<&str> = picked.iter().map(|s| s.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "c"], "only the unqueued survive");
    }

    // ---------------------------------------------------------------- gapless

    #[test]
    fn playing_hands_the_player_a_window_of_tracks() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue
            .replace((0..5).map(|i| track(&format!("s{i}"))).collect());

        app.play_current();

        assert_eq!(app.now.playlist_len, PLAYLIST_WINDOW);
        assert_eq!(app.now.playlist_base, 0);
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s0"));
    }

    #[test]
    fn following_the_playlist_moves_the_queue_without_a_reload() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue
            .replace((0..5).map(|i| track(&format!("s{i}"))).collect());
        app.play_current();

        // What mpv reports when it moves on by itself.
        app.follow_playlist(1);

        assert_eq!(app.queue.position(), Some(1));
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s1"));
        assert!(app.now.loaded && !app.now.paused);
        assert_eq!(app.now.position, 0.0, "the clock restarts with the track");
    }

    #[test]
    fn the_window_is_extended_before_it_runs_out() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue
            .replace((0..10).map(|i| track(&format!("s{i}"))).collect());
        app.play_current();
        assert_eq!(app.now.playlist_len, PLAYLIST_WINDOW);

        // Reaching the last entry of the window must queue more.
        app.follow_playlist(PLAYLIST_WINDOW - 1);
        assert_eq!(app.now.playlist_len, PLAYLIST_WINDOW * 2);
    }

    #[test]
    fn an_exhausted_playlist_advances_and_reopens_a_window() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b")]);

        app.play_current();
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("a"));

        app.on_playlist_finished();
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("b"));
    }

    #[test]
    fn the_end_of_the_queue_stops_instead_of_looping() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("only")]);
        app.play_current();

        app.on_playlist_finished();
        assert!(!app.now.loaded, "nothing is loaded at the end of the queue");
        assert!(app.now.paused);
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("end of queue"), "{}", status.text);
    }

    #[test]
    fn the_player_tells_the_app_which_entry_it_moved_to() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue
            .replace((0..5).map(|i| track(&format!("s{i}"))).collect());
        app.play_current();

        // The exact event mpv sends on a gapless transition.
        app.on_player_event(PlayerEvent::PlaylistPos(2));

        assert_eq!(app.queue.position(), Some(2));
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s2"));
    }

    #[test]
    fn the_end_of_each_file_does_not_advance_the_queue() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue
            .replace((0..5).map(|i| track(&format!("s{i}"))).collect());
        app.play_current();

        // mpv reports the end of every entry; the playlist position is what
        // marks the transition, so this must be inert.
        app.on_player_event(PlayerEvent::Ended(EndReason::Eof));

        assert_eq!(app.queue.position(), Some(0));
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s0"));
    }

    #[test]
    fn a_failed_track_is_reported_rather_than_silently_skipped() {
        let mut app = app();
        app.on_player_event(PlayerEvent::Ended(EndReason::Error));
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("failed to play"), "{}", status.text);
        assert!(status.error);
    }

    #[test]
    fn choosing_a_playlist_track_queues_the_whole_playlist_from_there() {
        let mut app = app();
        app.settings.auto_dj = false;
        let mut playlist = Playlist {
            id: "pl1".into(),
            name: "morning".into(),
            ..Default::default()
        };
        playlist.entry = (0..4)
            .map(|i| Song {
                id: format!("s{i}"),
                title: format!("song {i}"),
                duration: Some(120),
                ..Default::default()
            })
            .collect();
        app.playlist_detail = Some(playlist);

        app.play_playlist_track(2);

        assert_eq!(app.queue.len(), 4, "the whole playlist is queued");
        assert_eq!(app.queue.position(), Some(2));
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s2"));
    }

    #[test]
    fn an_empty_playlist_says_so_instead_of_playing_nothing() {
        let mut app = app();
        app.playlist_detail = Some(Playlist {
            id: "pl1".into(),
            name: "empty".into(),
            ..Default::default()
        });

        app.play_playlist_track(0);

        assert!(app.queue.is_empty());
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("empty"), "{}", status.text);
    }

    #[test]
    fn typing_a_query_does_not_trigger_commands() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('/')));

        // Every one of these is a command on the album list.
        for c in ['q', 'd', 'x', 'a', 's', 'r'] {
            app.handle_key(key(KeyCode::Char(c)));
        }

        assert_eq!(app.search.query, "qdxasr");
        assert!(!app.should_quit, "typing q must not quit");
        assert!(app.queue.is_empty(), "typing never queues anything");
    }

    #[test]
    fn backspace_edits_and_escape_leaves_the_box() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('/')));
        app.handle_key(key(KeyCode::Char('a')));
        app.handle_key(key(KeyCode::Char('b')));
        app.handle_key(key(KeyCode::Backspace));

        assert_eq!(app.search.query, "a");
        assert!(app.search.typing);

        app.handle_key(key(KeyCode::Esc));
        assert!(!app.search.typing, "escape closes the box");
        assert_eq!(app.search.query, "a", "the query is kept for editing");
    }

    #[test]
    fn a_song_result_plays_and_leaves_the_album_list_alone() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.search.hits = vec![SearchHit::Album(Album {
            id: "al1".into(),
            name: "Kid A".into(),
            ..Default::default()
        })];
        app.search.hits.push(SearchHit::Song(Song {
            id: "s1".into(),
            title: "Everything In Its Right Place".into(),
            duration: Some(240),
            ..Default::default()
        }));
        app.view = View::Search;
        app.search.selected = 1;

        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.queue.len(), 1);
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s1"));
    }

    #[test]
    fn auto_dj_stays_out_of_the_way_when_repeat_is_on() {
        let mut app = app();
        app.settings.auto_dj = true;
        app.now.track = Some(track("seed"));
        app.now.loaded = true;
        app.queue.replace(vec![track("seed")]);
        app.queue.set_repeat(Repeat::All);

        app.auto_dj_top_up();

        // Repeat means the queue never runs out; topping it up would grow it
        // without bound and reorder what the user asked for.
        assert_eq!(app.queue.len(), 1);
        assert!(app.auto_dj_seed.is_none(), "and it must not even ask");
    }

    #[test]
    fn a_startup_idle_from_the_player_does_not_start_playing() {
        // mpv is spawned with `--idle=yes`, so the first thing it reports is
        // that it is idle. Treating that as the end of a queue made the app
        // start playing on launch, and even step past the restored track.
        let mut app = app();
        app.settings.auto_dj = false;
        app.restore_session(&Session {
            queue: vec![track("a"), track("b"), track("c")],
            position: 0,
            volume: None,
            seconds: 12.0,
            ..Default::default()
        });

        app.on_player_event(PlayerEvent::Idle);

        assert_eq!(app.queue.position(), Some(0), "the cursor must not move");
        assert!(!app.now.loaded, "nothing was loaded");
        assert!(app.now.paused, "and nothing starts on its own");
        assert_eq!(
            app.now.position, 12.0,
            "the restored position is still the one shown"
        );
        assert_eq!(
            app.now.track.as_ref().map(|t| t.id.as_str()),
            Some("a"),
            "and the bar still names the track that was restored"
        );
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("restored"), "{}", status.text);
    }

    #[test]
    fn the_players_initial_pause_state_does_not_override_a_restored_one() {
        let mut app = app();
        app.restore_session(&Session {
            queue: vec![track("a")],
            position: 0,
            volume: None,
            seconds: 30.0,
            ..Default::default()
        });

        // A fresh mpv reports `pause=no`, which would draw a play marker over
        // a track that is not playing.
        app.on_player_event(PlayerEvent::Paused(false));

        assert!(app.now.paused, "a restored track stays paused");
    }

    #[test]
    fn an_idle_after_playback_still_advances_the_queue() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b")]);
        app.play_current();

        app.on_player_event(PlayerEvent::Idle);

        assert_eq!(
            app.now.track.as_ref().map(|t| t.id.as_str()),
            Some("b"),
            "the real end of a track still moves on"
        );
    }

    #[test]
    fn a_restored_track_keeps_its_place_in_the_track() {
        let mut app = app();
        app.restore_session(&Session {
            queue: vec![track("a"), track("b")],
            position: 0,
            volume: Some(40),
            seconds: 95.0,
            view: Some("queue".into()),
            ..Default::default()
        });

        // Shown, not loaded: the clock reflects where it stopped.
        assert_eq!(app.now.position, 95.0);
        assert!(!app.now.loaded);
        assert_eq!(app.pending_seek, Some(95.0));
        // And the list comes back too.
        assert_eq!(app.view, View::Queue);
    }

    #[test]
    fn starting_a_restored_track_seeks_to_where_it_stopped() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.restore_session(&Session {
            queue: vec![track("a")],
            position: 0,
            volume: None,
            seconds: 95.0,
            ..Default::default()
        });

        app.play_or_pause();

        // The seek is handed over once; a later track change must not reuse it.
        assert_eq!(app.pending_seek, None);
        assert_eq!(app.now.position, 95.0);
        assert!(app.now.loaded);
    }

    #[test]
    fn a_view_that_needs_a_request_opens_on_the_library_instead() {
        let mut app = app();
        app.restore_session(&Session {
            queue: vec![track("a")],
            position: 0,
            volume: None,
            view: Some("album".into()),
            ..Default::default()
        });

        assert_eq!(app.view, View::Albums, "an album detail is not restorable");
    }

    #[test]
    fn w_turns_the_artist_list_into_a_wall() {
        let mut app = app();
        app.view = View::Artists;
        app.artists = (0..6)
            .map(|i| Artist {
                id: format!("ar-{i}"),
                name: format!("artist {i}"),
                cover_art: Some(format!("ar-{i}")),
                ..Default::default()
            })
            .collect();

        app.handle_key(key(KeyCode::Char('w')));

        assert_eq!(app.artist_layout, crate::app::AlbumLayout::Grid);
        assert_eq!(app.artist_scroll, 0);
    }

    #[test]
    fn the_artist_wall_leaves_the_album_layout_alone() {
        let mut app = app();
        app.view = View::Artists;
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(app.layout, crate::app::AlbumLayout::List);
    }

    #[test]
    fn every_album_list_has_its_own_wall() {
        let mut app = app();
        app.favorites = vec![Album {
            id: "al-1".into(),
            name: "Starred".into(),
            ..Default::default()
        }];
        app.listen_albums = vec![Album {
            id: "al-2".into(),
            name: "Kept".into(),
            ..Default::default()
        }];

        // Starred.
        app.view = View::Favorites;
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(app.favorites_layout, AlbumLayout::Grid);
        assert_eq!(
            app.later_layout,
            AlbumLayout::List,
            "only this list changes"
        );

        // Later.
        app.view = View::ListenLater;
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(app.later_layout, AlbumLayout::Grid);
        assert_eq!(app.layout, AlbumLayout::List, "and not the library");
    }

    #[test]
    fn a_view_with_no_wall_says_so() {
        let mut app = app();
        app.view = View::Queue;
        app.handle_key(key(KeyCode::Char('w')));
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("no wall"), "{}", status.text);
    }

    // ------------------------------------------------------------ queue cursor

    #[test]
    fn moving_in_the_queue_does_not_change_what_plays() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b"), track("c")]);
        app.play_current();
        app.view = View::Queue;

        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));

        assert_eq!(app.queue_cursor(), 2, "the cursor went where it was told");
        assert_eq!(
            app.queue.position(),
            Some(0),
            "browsing the queue must not move playback"
        );
        assert_eq!(
            app.now.track.as_ref().map(|t| t.id.as_str()),
            Some("a"),
            "and what is playing stays playing"
        );
    }

    #[test]
    fn x_removes_the_selected_row_and_leaves_playback_alone() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b"), track("c")]);
        app.play_current();
        app.view = View::Queue;
        app.handle_key(key(KeyCode::Char('j')));
        app.handle_key(key(KeyCode::Char('j')));

        app.handle_key(key(KeyCode::Char('x')));

        let ids: Vec<&str> = app
            .queue
            .playback_items()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(app.queue_cursor(), 1, "the cursor closes the gap");
        assert_eq!(
            app.now.track.as_ref().map(|t| t.id.as_str()),
            Some("a"),
            "what is playing keeps playing"
        );
    }

    #[test]
    fn x_on_the_playing_row_starts_what_replaces_it() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b"), track("c")]);
        app.play_current();
        app.view = View::Queue;

        app.handle_key(key(KeyCode::Char('x')));

        assert_eq!(app.queue.len(), 2);
        assert_eq!(
            app.now.track.as_ref().map(|t| t.id.as_str()),
            Some("b"),
            "the next row takes over"
        );
        assert_eq!(app.queue.position(), Some(0));

        // Removing the last one stops playback instead of leaving a ghost.
        app.handle_key(key(KeyCode::Char('x')));
        app.handle_key(key(KeyCode::Char('x')));
        assert!(app.queue.is_empty());
        assert!(app.now.track.is_none(), "nothing is playing any more");
        assert!(!app.now.loaded);
    }

    #[test]
    fn capital_j_and_k_reorder_the_queue_and_carry_the_cursor() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b"), track("c")]);
        app.play_current();
        app.view = View::Queue;

        app.handle_key(key(KeyCode::Char('J')));
        app.handle_key(key(KeyCode::Char('J')));
        let ids: Vec<&str> = app
            .queue
            .playback_items()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "c", "a"]);
        assert_eq!(
            app.now.track.as_ref().map(|t| t.id.as_str()),
            Some("a"),
            "reordering does not change the track"
        );

        app.handle_key(key(KeyCode::Char('K')));
        let ids: Vec<&str> = app
            .queue
            .playback_items()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["b", "a", "c"]);
        assert_eq!(app.queue_cursor(), 1, "the carried row is still selected");
    }

    #[test]
    fn enter_in_the_queue_starts_the_row_under_the_cursor() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b"), track("c")]);
        app.play_current();
        app.view = View::Queue;
        app.handle_key(key(KeyCode::Char('j')));

        app.handle_key(key(KeyCode::Enter));

        assert_eq!(app.queue.position(), Some(1));
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("b"));
        assert_eq!(
            app.queue_cursor(),
            1,
            "after Enter the cursor follows the track it started"
        );
    }

    #[test]
    fn opening_the_queue_shows_the_track_that_is_playing() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b")]);
        app.play_current();
        app.view = View::Queue;
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.queue_cursor(), 0);

        app.jump_to_view(View::Albums);
        app.jump_to_view(View::Queue);

        assert_eq!(app.queue_cursor(), 0, "back on the playing row");
    }

    #[test]
    fn a_skipped_track_takes_the_cursor_with_it() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.queue.replace(vec![track("a"), track("b")]);
        app.play_current();
        app.view = View::Queue;

        app.next_track();

        assert_eq!(app.queue.position(), Some(1));
        assert_eq!(app.queue_cursor(), 1);
    }

    #[test]
    fn the_walls_are_remembered_separately() {
        let mut before = app();
        before.favorites_layout = AlbumLayout::Grid;
        before.later_layout = AlbumLayout::List;
        // The artist list has its own wall and must survive a restart too.
        before.artist_layout = AlbumLayout::Grid;
        let saved = before.session_snapshot();

        let mut fresh = app();
        fresh.restore_session(&saved);

        assert_eq!(fresh.favorites_layout, AlbumLayout::Grid);
        assert_eq!(fresh.later_layout, AlbumLayout::List);
        assert_eq!(fresh.artist_layout, AlbumLayout::Grid);
    }

    #[test]
    fn enter_plays_the_track_under_the_cursor() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.detail = Some(Album {
            id: "al-1".into(),
            name: "Album".into(),
            song: (0..4)
                .map(|i| Song {
                    id: format!("t{i}"),
                    title: format!("track {i}"),
                    duration: Some(100),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        });
        app.view = View::AlbumDetail;

        // Every row, in order: the cursor indexes the track list directly, so
        // Enter must never play the track before the one under it.
        for index in 0..4 {
            app.detail_selected = index;
            app.handle_key(key(KeyCode::Enter));
            assert_eq!(
                app.now.track.as_ref().map(|track| track.id.clone()),
                Some(format!("t{index}")),
                "Enter on row {index} played the wrong track"
            );
        }
    }

    // ---------------------------------------------------------------- history

    #[test]
    fn l_goes_back_to_where_h_came_from() {
        let mut app = app();
        app.detail = Some(Album {
            id: "al-1".into(),
            name: "Album".into(),
            ..Default::default()
        });
        app.set_view(View::AlbumDetail);

        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.view, View::Albums);
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.view, View::AlbumDetail, "l undoes h");
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.view, View::Albums);
    }

    #[test]
    fn l_on_a_wall_leaves_the_view_alone() {
        let mut app = app();
        app.view = View::Albums;
        app.layout = AlbumLayout::Grid;
        app.albums = (0..6)
            .map(|i| Album {
                id: format!("al-{i}"),
                name: format!("album {i}"),
                ..Default::default()
            })
            .collect();
        app.list_area = (100, 20);
        app.selected = 1;

        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.view, View::Albums, "the wall keeps l");
        assert_eq!(app.selected, 2, "and l moves between covers");

        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.selected, 1);
        assert_eq!(app.view, View::Albums);
    }

    #[test]
    fn a_rail_jump_forgets_the_way_forward() {
        let mut app = app();
        app.detail = Some(Album {
            id: "al-1".into(),
            name: "Album".into(),
            ..Default::default()
        });
        app.set_view(View::AlbumDetail);
        app.handle_key(key(KeyCode::Char('h'))); // now on Albums, forward=AlbumDetail

        app.handle_key(key(KeyCode::Char('2'))); // rail: artists
        app.handle_key(key(KeyCode::Char('l')));

        assert_ne!(app.view, View::AlbumDetail, "rail navigation starts over");
    }

    #[test]
    fn the_arrows_walk_history_too() {
        let mut app = app();
        app.detail = Some(Album {
            id: "al-1".into(),
            name: "Album".into(),
            ..Default::default()
        });
        app.set_view(View::AlbumDetail);

        app.handle_key(key(KeyCode::Left));
        assert_eq!(app.view, View::Albums);
        app.handle_key(key(KeyCode::Right));
        assert_eq!(app.view, View::AlbumDetail);
    }

    #[test]
    fn t_switches_the_left_pane_to_top_songs() {
        let mut app = artist_page_app();
        app.artist_songs = (0..4)
            .map(|i| Song {
                id: format!("s{i}"),
                title: format!("top {i}"),
                duration: Some(180),
                ..Default::default()
            })
            .collect();

        app.handle_key(key(KeyCode::Char('t')));

        assert_eq!(app.artist_left, ArtistLeft::Songs);
        assert_eq!(app.artist_pane, ArtistPane::Albums, "focus follows the tab");
        assert_eq!(app.cursor_len(), 4, "the cursor walks the songs now");
    }

    #[test]
    fn a_top_song_queues_the_rest_of_them() {
        let mut app = app();
        app.settings.auto_dj = false;
        app.artist_songs = (0..3)
            .map(|i| Song {
                id: format!("s{i}"),
                title: format!("top {i}"),
                duration: Some(120),
                ..Default::default()
            })
            .collect();

        app.play_artist_song(1);

        assert_eq!(app.queue.len(), 3, "the whole list is queued");
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("s1"));
    }

    #[test]
    fn an_artist_with_no_top_songs_says_so() {
        let mut app = app();
        app.play_artist_song(0);
        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("no top songs"), "{}", status.text);
    }

    // ------------------------------------------------------------- artist page

    fn artist_page_app() -> App {
        let mut app = app();
        app.artist_detail = Some(ArtistDetail {
            id: "ar-1".into(),
            name: "Aphex Twin".into(),
            album: vec![Album {
                id: "al-1".into(),
                name: "Selected Ambient Works".into(),
                artist_id: Some("ar-1".into()),
                artist: Some("Aphex Twin".into()),
                ..Default::default()
            }],
            ..Default::default()
        });
        app.artist_info = Some(ArtistInfo {
            biography: Some("Richard D. James.\nSecond paragraph.".into()),
            similar_artist: vec![
                Artist {
                    id: "ar-2".into(),
                    name: "Boards of Canada".into(),
                    ..Default::default()
                },
                Artist {
                    id: "ar-3".into(),
                    name: "Autechre".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        app.view = View::ArtistDetail;
        app
    }

    #[test]
    fn tab_moves_between_the_artist_panes() {
        let mut app = artist_page_app();
        assert_eq!(app.artist_pane, ArtistPane::Albums);

        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.artist_pane, ArtistPane::Similar);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.artist_pane, ArtistPane::Biography);
        app.handle_key(key(KeyCode::Tab));
        assert_eq!(app.artist_pane, ArtistPane::Albums);
        // And backwards.
        app.handle_key(key(KeyCode::BackTab));
        assert_eq!(app.artist_pane, ArtistPane::Biography);
    }

    #[test]
    fn the_cursor_follows_the_focused_pane() {
        let mut app = artist_page_app();
        app.handle_key(key(KeyCode::Tab)); // similar
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.artist_similar_selected, 1);
        assert_eq!(app.artist_album_selected, 0, "the album cursor stays put");

        app.handle_key(key(KeyCode::Tab)); // biography
                                           // The renderer normally fills these in; a bare App in a test has not
                                           // drawn yet, and without them there is nothing to scroll.
        app.artist_bio_lines = 40;
        app.artist_bio_page = 5;
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.artist_bio_scroll, 1);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.artist_bio_scroll, 0, "and k walks back up");
    }

    #[test]
    fn a_biography_has_nothing_to_open() {
        let mut app = artist_page_app();
        app.handle_key(key(KeyCode::Tab));
        app.handle_key(key(KeyCode::Tab)); // biography
        app.handle_key(key(KeyCode::Enter));

        // Still on the artist page: Enter did not navigate anywhere.
        assert_eq!(app.view, View::ArtistDetail);
    }

    #[test]
    fn i_names_the_artist_of_the_open_album() {
        let mut app = app();
        app.detail = Some(Album {
            id: "al-1".into(),
            name: "No Artist Field".into(),
            artist_id: None,
            ..Default::default()
        });
        app.view = View::AlbumDetail;

        app.handle_key(key(KeyCode::Char('i')));

        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("names no artist"), "{}", status.text);
    }

    #[test]
    fn a_and_big_a_queue_the_selection_after_the_current_and_at_the_end() {
        let mut app = app();
        app.detail = Some(album("x", 3));
        app.view = View::AlbumDetail;
        app.detail_selected = 1;
        app.queue.replace(vec![track("q1"), track("q2")]);

        app.handle_key(key(KeyCode::Char('a')));
        let ids: Vec<&str> = app
            .queue
            .playback_items()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["q1", "x-1", "q2"], "a tucks it in next");

        app.handle_key(key(KeyCode::Char('A')));
        let ids: Vec<&str> = app
            .queue
            .playback_items()
            .iter()
            .map(|item| item.id.as_str())
            .collect();
        assert_eq!(ids, vec!["q1", "x-1", "q2", "x-1"], "A adds at the end");
        assert_eq!(app.queue.items()[0].id, "q1", "the playing track stayed");
    }

    #[test]
    fn sort_and_layout_survive_a_restart() {
        let mut before = app();
        before.sort = crate::subsonic::AlbumListType::AlphabeticalByName;
        before.artist_sort = ArtistSort::Name;
        before.layout = AlbumLayout::Grid;
        let saved = before.session_snapshot();

        let mut fresh = app();
        fresh.restore_session(&saved);

        assert_eq!(
            fresh.sort,
            crate::subsonic::AlbumListType::AlphabeticalByName
        );
        assert_eq!(fresh.artist_sort, ArtistSort::Name);
        assert_eq!(fresh.layout, AlbumLayout::Grid);
    }

    #[test]
    fn a_session_without_sort_keys_keeps_the_defaults() {
        let mut fresh = app();
        let before = (fresh.sort, fresh.artist_sort, fresh.layout);

        fresh.restore_session(&Session {
            queue: vec![track("a")],
            ..Default::default()
        });

        assert_eq!(
            (fresh.sort, fresh.artist_sort, fresh.layout),
            before,
            "an old session file must not reset anything"
        );
    }

    // --------------------------------------------------------------- session

    fn session_of(ids: &[&str], position: usize) -> Session {
        Session {
            queue: ids.iter().map(|id| track(id)).collect(),
            position,
            volume: Some(42),
            ..Default::default()
        }
    }

    #[test]
    fn restoring_shows_the_track_without_playing_it() {
        let mut app = app();
        app.restore_session(&session_of(&["a", "b"], 1));

        assert_eq!(app.queue.len(), 2);
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("b"));
        assert!(app.now.paused, "a restored track must not be playing");
        assert!(!app.now.loaded, "nothing is streamed until asked");
        assert_eq!(app.volume, 42, "volume comes back too");
    }

    #[test]
    fn a_restored_track_does_not_accumulate_listening_time() {
        let mut app = app();
        app.restore_session(&session_of(&["a"], 0));
        let before = app.now.listened;
        app.tick();
        assert_eq!(app.now.listened, before, "silence is not listening");
    }

    #[test]
    fn a_position_past_the_end_restores_the_last_track() {
        let mut app = app();
        app.restore_session(&session_of(&["a", "b", "c"], 99));
        assert_eq!(app.now.track.as_ref().map(|t| t.id.as_str()), Some("c"));
    }

    #[test]
    fn an_empty_session_restores_nothing() {
        let mut app = app();
        app.restore_session(&Session::default());
        assert!(app.queue.is_empty());
        assert!(app.now.track.is_none());
    }

    #[test]
    fn snapshot_captures_the_queue_and_position() {
        let mut app = app();
        app.queue.replace(vec![track("a"), track("b")]);
        app.queue.jump_to(1);
        app.volume = 55;

        let snapshot = app.session_snapshot();
        assert_eq!(snapshot.queue.len(), 2);
        assert_eq!(snapshot.position, 1);
        assert_eq!(snapshot.volume, Some(55));
    }

    #[test]
    fn h_leaves_listen_later_again() {
        let mut app = app();
        app.albums = vec![album("a", 1)];
        app.handle_key(key(KeyCode::Char('4')));
        assert_eq!(app.view, View::ListenLater);
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.view, View::Albums);
    }

    // -------------------------------------------------------------- navigation

    fn artist(id: &str, name: &str) -> Artist {
        Artist {
            id: id.to_string(),
            name: name.to_string(),
            album_count: Some(2),
            ..Default::default()
        }
    }

    #[test]
    fn the_back_stack_returns_to_where_you_came_from() {
        let mut app = app();
        app.set_view(View::Artists);
        app.set_view(View::ArtistDetail);

        app.go_back();
        assert_eq!(
            app.view,
            View::Artists,
            "an artist page goes back to artists"
        );
        app.go_back();
        assert_eq!(app.view, View::Albums);

        // At the library there is nothing to pop; it must not wander.
        app.go_back();
        assert_eq!(app.view, View::Albums);
    }

    #[test]
    fn h_and_l_walk_artist_to_artist_without_losing_either_page() {
        let mut app = app();
        app.artist_detail = Some(ArtistDetail {
            id: "ar-1".into(),
            name: "First".into(),
            album: vec![album("a", 1)],
            ..Default::default()
        });
        app.artist_info = Some(ArtistInfo {
            biography: Some("the first biography".into()),
            ..Default::default()
        });
        app.view = View::ArtistDetail;

        // Standing in for a similar-artist jump: the old page is saved before
        // the new one overwrites it.
        app.nav_stack.push(app.location());
        app.artist_detail = Some(ArtistDetail {
            id: "ar-2".into(),
            name: "Second".into(),
            ..Default::default()
        });
        app.artist_info = None;

        app.go_back();
        assert_eq!(app.artist_detail.as_ref().unwrap().id, "ar-1");
        assert_eq!(
            app.artist_info
                .as_ref()
                .and_then(|info| info.biography.as_deref()),
            Some("the first biography"),
            "the page comes back whole, not just its id"
        );
        assert_eq!(app.artist_detail.as_ref().unwrap().album.len(), 1);

        app.go_forward();
        assert_eq!(app.artist_detail.as_ref().unwrap().id, "ar-2");
    }

    #[test]
    fn two_opens_the_artist_list_without_refetching() {
        let mut app = app();
        app.artists = vec![artist("ar-1", "Aphex Twin")];
        app.handle_key(key(KeyCode::Char('2')));

        assert_eq!(app.view, View::Artists);
        // The server in tests does not exist, so reaching it would have set an
        // error: a pre-filled list must not be refetched.
        assert!(app.status.is_none(), "no request should have been made");
    }

    #[test]
    fn artists_navigate_with_the_vim_keys() {
        let mut app = app();
        app.artists = vec![artist("1", "A"), artist("2", "B"), artist("3", "C")];
        app.view = View::Artists;

        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.artist_selected, 1);
        app.handle_key(key(KeyCode::Char('G')));
        assert_eq!(app.artist_selected, 2);
        app.handle_key(key(KeyCode::Char('g')));
        assert_eq!(app.artist_selected, 0);
        app.handle_key(key(KeyCode::Char('k')));
        assert_eq!(app.artist_selected, 0, "clamped at the top");
    }

    #[test]
    fn starring_nothing_says_so_instead_of_calling_the_server() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('*')));

        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("no album"), "{}", status.text);
        assert!(!status.error, "an empty cursor is not an error");
    }

    // ------------------------------------------------------------------ sorts

    #[test]
    fn artists_sort_by_album_count_then_name() {
        let mut app = app();
        app.artists = vec![
            Artist {
                id: "1".into(),
                name: "beta".into(),
                album_count: Some(1),
                ..Default::default()
            },
            Artist {
                id: "2".into(),
                name: "Alpha".into(),
                album_count: Some(5),
                ..Default::default()
            },
            Artist {
                id: "3".into(),
                name: "alpha".into(),
                album_count: Some(5),
                ..Default::default()
            },
        ];
        app.artist_sort = ArtistSort::MostAlbums;
        app.sort_artists();

        let names: Vec<&str> = app.artists.iter().map(|a| a.name.as_str()).collect();
        // Equal counts fall back to a case-insensitive name comparison.
        assert_eq!(names, vec!["Alpha", "alpha", "beta"]);
    }

    #[test]
    fn artists_sort_by_name_case_insensitively() {
        let mut app = app();
        app.artists = vec![
            Artist {
                id: "1".into(),
                name: "zed".into(),
                ..Default::default()
            },
            Artist {
                id: "2".into(),
                name: "Alpha".into(),
                ..Default::default()
            },
        ];
        app.artist_sort = ArtistSort::Name;
        app.sort_artists();
        let names: Vec<&str> = app.artists.iter().map(|a| a.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "zed"]);
    }

    #[test]
    fn most_played_uses_the_frequent_ranking_and_puts_unknown_artists_last() {
        let mut app = app();
        app.artists = vec![
            Artist {
                id: "unranked".into(),
                name: "aaa".into(),
                ..Default::default()
            },
            Artist {
                id: "second".into(),
                name: "zzz".into(),
                ..Default::default()
            },
            Artist {
                id: "first".into(),
                name: "mmm".into(),
                ..Default::default()
            },
        ];
        // Pre-filled rank map: choosing this sort must not need a request.
        app.artist_play_rank.insert("first".into(), 0);
        app.artist_play_rank.insert("second".into(), 1);
        app.artist_sort = ArtistSort::MostPlayed;
        app.sort_artists();

        let ids: Vec<&str> = app.artists.iter().map(|a| a.id.as_str()).collect();
        assert_eq!(ids, vec!["first", "second", "unranked"]);
        assert!(app.status.is_none(), "a filled rank map needs no fetch");
    }

    #[test]
    fn s_cycles_the_artist_sort_without_touching_the_albums_sort() {
        let mut app = app();
        app.artists = vec![artist("1", "A"), artist("2", "B")];
        app.view = View::Artists;
        let album_sort = app.sort;

        app.handle_key(key(KeyCode::Char('S')));
        assert_eq!(app.artist_sort, ArtistSort::Name, "most albums -> a-z");
        assert_eq!(app.sort, album_sort, "the album sort is untouched");

        let status = app.status.as_ref().expect("a message");
        assert!(status.text.contains("artists: a-z"), "{}", status.text);
    }

    #[test]
    fn the_rail_keys_jump_without_depending_on_the_back_stack() {
        let mut app = app();
        app.albums = vec![album("a", 1)];
        // Drill in, then use the rail: back must not return to the detail.
        app.set_view(View::AlbumDetail);
        app.handle_key(key(KeyCode::Char('4')));
        assert_eq!(app.view, View::ListenLater);
        assert!(app.nav_stack.is_empty(), "a rail jump resets the stack");
        app.handle_key(key(KeyCode::Char('h')));
        assert_eq!(app.view, View::Albums);
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
    fn closing_help_does_not_leave_it_in_the_forward_history() {
        let mut app = app();
        app.handle_key(key(KeyCode::Char('?')));
        assert_eq!(app.view, View::Help);
        app.handle_key(key(KeyCode::Char('j')));
        assert_eq!(app.view, View::Albums);
        app.handle_key(key(KeyCode::Char('l')));
        assert_ne!(app.view, View::Help, "l goes forward, not back into help");
    }

    #[test]
    fn help_leaves_the_forward_history_alone() {
        let mut app = app();
        app.detail = Some(album("al-1", 1));
        app.set_view(View::AlbumDetail);
        app.handle_key(key(KeyCode::Char('h'))); // Albums, with the album ahead

        app.handle_key(key(KeyCode::Char('?')));
        app.handle_key(key(KeyCode::Char('j'))); // closes help
        app.handle_key(key(KeyCode::Char('l')));
        assert_eq!(app.view, View::AlbumDetail, "help is a dead-end modal");
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
            Some("albums: wall")
        );
        app.handle_key(key(KeyCode::Char('w')));
        assert_eq!(app.layout, AlbumLayout::List);
        assert_eq!(
            app.status.as_ref().map(|s| s.text.as_str()),
            Some("albums: list")
        );
    }

    #[test]
    fn grid_navigation_moves_by_rows_and_cells() {
        let mut app = app();
        app.layout = AlbumLayout::Grid;
        app.list_area = (100, 30);
        app.albums = (0..40).map(|i| album(&i.to_string(), 1)).collect();
        let cols = app.wall(100, 30).cols;
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
