//! Rendering.
//!
//! Two-phase by design: `collect_placements` runs before the frame and decides
//! where images go (and requests any art that is missing), then the painter
//! emits erases, the text frame is drawn, and the painter emits paints. That
//! ordering is what stops an erase from wiping text the diff just wrote.

use std::io::{self, Stdout, Write};

use crossterm::cursor::MoveTo;
use crossterm::queue;
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;
use ratatui::Terminal;

use crate::app::{AlbumLayout, App, ArtistLeft, ArtistPane, View, ROW_HEIGHT};
use crate::art::ArtService;
use crate::render::{
    blank_sixel, kitty_delete, kitty_id, ArtProtocol, CellSize, FramePlan, PaintOp, Placement,
};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

const ACCENT: Color = Color::Magenta;
const MUTED: Color = Color::DarkGray;

fn title_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn selected_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD).fg(ACCENT)
}

/// A track that is playing without being selected: the accent colour says it is
/// making sound, without the bold that says Enter goes here.
fn playing_style() -> Style {
    Style::default().fg(ACCENT)
}

/// Rows the now-playing bar occupies, top border included: two lines beside
/// the cover — track and clock, then progress plus album/format/next.
/// Rows the bottom bar occupies, top border included: a title line and a
/// progress line for the track, state and keys, then the queue preview.
pub const BAR_HEIGHT: u16 = 5;

/// The album page's cover: as wide as this, and never taller than the block
/// of text beside it.
const COVER_W: u16 = 20;
const COVER_SPACE_H: u16 = 9;

/// Width of the navigation rail, and the narrowest window that keeps it.
pub const SIDEBAR_W: u16 = 12;
pub const SIDEBAR_MIN_WIDTH: u16 = 64;
/// One column of rule between the rail and the view, then one of air.
const SIDEBAR_GAP: u16 = 2;
/// One row above every view for its title and sort.
const HEADER_HEIGHT: u16 = 1;

/// Where each part of the screen lives.
struct Shell {
    /// The navigation rail, absent on narrow terminals.
    sidebar: Option<Rect>,
    /// The rule that separates the rail from the view.
    divider: Option<Rect>,
    /// View title and sort, above the view's own content.
    header: Rect,
    /// The list itself.
    body: Rect,
    /// Always present: it carries state, keys and the queue.
    bar: Rect,
}

/// Split the screen into [sidebar | header + body], and the bar.
fn shell_layout(area: Rect) -> Shell {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(4), Constraint::Length(BAR_HEIGHT)])
        .split(area);
    let top = rows[0];

    let (sidebar, divider, right) = if area.width < SIDEBAR_MIN_WIDTH {
        (None, None, top)
    } else {
        let columns = Layout::default()
            .direction(Direction::Horizontal)
            .constraints([
                Constraint::Length(SIDEBAR_W + SIDEBAR_GAP),
                Constraint::Min(20),
            ])
            .split(top);
        (
            Some(Rect {
                width: SIDEBAR_W,
                ..columns[0]
            }),
            Some(Rect {
                x: columns[0].x + SIDEBAR_W,
                width: 1,
                ..columns[0]
            }),
            columns[1],
        )
    };

    let header = Rect {
        height: HEADER_HEIGHT.min(right.height),
        ..right
    };
    Shell {
        sidebar,
        divider,
        header,
        body: Rect {
            y: right.y + header.height,
            height: right.height.saturating_sub(header.height),
            ..right
        },
        bar: rows[1],
    }
}

/// The views reachable from the rail, with their jump keys.
const NAV_ITEMS: [(View, &str, char); 6] = [
    (View::Albums, "Albums", '1'),
    (View::Artists, "Artists", '2'),
    (View::Favorites, "Starred", '3'),
    (View::ListenLater, "Later", '4'),
    (View::Playlists, "Lists", '5'),
    (View::Queue, "Queue", '6'),
];

/// Which rail entry a view belongs to: an album detail is still "Albums".
fn nav_owner(view: View) -> View {
    match view {
        View::AlbumDetail => View::Albums,
        View::ArtistDetail => View::Artists,
        View::PlaylistDetail => View::Playlists,
        other => other,
    }
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let current = nav_owner(app.view);
    for (index, (view, label, key)) in NAV_ITEMS.iter().enumerate() {
        let y = area.y + index as u16;
        if y >= area.y + area.height {
            break;
        }
        let active = *view == current;
        let row = Rect {
            x: area.x,
            y,
            width: area.width,
            height: 1,
        };
        let line = Line::from(vec![
            Span::styled(
                if active { "▸ " } else { "  " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                format!("{label:<9}"),
                if active {
                    Style::default().fg(ACCENT).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(MUTED)
                },
            ),
            Span::styled(
                key.to_string(),
                if active {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default().fg(MUTED)
                },
            ),
        ]);
        frame.render_widget(Paragraph::new(line), row);
    }
}

/// Where an artist's albums start, below the two-row header.
fn artist_albums_area(area: Rect) -> Rect {
    let header = ROW_HEIGHT.min(area.height);
    Rect {
        y: area.y + header,
        height: area.height.saturating_sub(header),
        ..area
    }
}

fn visible_albums(area: Rect) -> usize {
    (area.height / ROW_HEIGHT).max(1) as usize
}

fn album_row_rect(area: Rect, index_in_view: usize) -> Rect {
    Rect {
        x: area.x,
        y: area.y + index_in_view as u16 * ROW_HEIGHT,
        width: area.width,
        height: ROW_HEIGHT,
    }
}

/// Work out image placements and request any art that is missing.
///
/// Returns nothing: the result is stored in `app.placements` for the painter.
pub fn collect_placements(app: &mut App, area: Rect) {
    app.placements.clear();
    if matches!(app.protocol, ArtProtocol::Off) {
        return;
    }

    let shell = shell_layout(area);
    let main = shell.body;
    app.list_area = (main.width, main.height);

    match app.view {
        View::Albums => {
            app.ensure_visible(main.width, main.height);
            match app.layout {
                AlbumLayout::List => {
                    let visible = visible_albums(main);
                    let covers: Vec<Option<String>> = app
                        .albums
                        .iter()
                        .map(|album| album.cover_art.clone())
                        .collect();
                    collect_list_placements(app, main, &covers, app.scroll, app.selected + visible);
                }
                AlbumLayout::Grid => {
                    let covers: Vec<Option<String>> = app
                        .albums
                        .iter()
                        .map(|album| album.cover_art.clone())
                        .collect();
                    collect_grid_placements(app, main, &covers, app.scroll, app.selected);
                }
            }
        }
        View::AlbumDetail => {
            if let Some(album) = app.detail.as_ref() {
                if let Some(cover) = album.cover_art.as_deref() {
                    let colors = app.settings.cover_colors;
                    // The cover's column, as draw_album_detail lays it out: the
                    // information sits one cell to its right.
                    let thumb = app.square_thumb(COVER_W, main.height.min(COVER_SPACE_H));
                    let key = app
                        .art
                        .request(cover, thumb.art_cols, thumb.art_rows, colors, false);
                    if app.art.is_ready(&key) {
                        app.placements
                            .push(Placement::new(key, main.y, main.x, thumb.rows, thumb.cols));
                    }
                }
            }
        }
        View::Artists => {
            app.ensure_artist_visible(main.width, main.height);
            let covers: Vec<Option<String>> = app
                .artists
                .iter()
                .map(|artist| artist.cover_art.clone())
                .collect();
            match app.artist_layout {
                AlbumLayout::List => collect_list_placements(
                    app,
                    main,
                    &covers,
                    app.artist_scroll,
                    app.artist_selected + visible_albums(main),
                ),
                AlbumLayout::Grid => collect_grid_placements(
                    app,
                    main,
                    &covers,
                    app.artist_scroll,
                    app.artist_selected,
                ),
            }
        }
        View::ArtistDetail => {
            let albums_area = artist_albums_area(main);
            app.ensure_artist_album_visible(albums_area.height);
            let covers: Vec<Option<String>> = app
                .artist_detail
                .as_ref()
                .map(|artist| {
                    artist
                        .album
                        .iter()
                        .map(|album| album.cover_art.clone())
                        .collect()
                })
                .unwrap_or_default();
            collect_list_placements(
                app,
                albums_area,
                &covers,
                app.artist_album_scroll,
                app.artist_album_selected + visible_albums(albums_area),
            );
        }
        View::Favorites => {
            app.ensure_favorite_visible(main.width, main.height);
            let covers: Vec<Option<String>> = app
                .favorites
                .iter()
                .map(|album| album.cover_art.clone())
                .collect();
            match app.favorites_layout {
                AlbumLayout::List => collect_list_placements(
                    app,
                    main,
                    &covers,
                    app.favorite_scroll,
                    app.favorite_selected + visible_albums(main),
                ),
                AlbumLayout::Grid => collect_grid_placements(
                    app,
                    main,
                    &covers,
                    app.favorite_scroll,
                    app.favorite_selected,
                ),
            }
        }
        View::ListenLater => {
            app.ensure_listen_visible(main.width, main.height);
            let visible = visible_albums(main);
            let covers: Vec<Option<String>> = app
                .listen_albums
                .iter()
                .map(|album| album.cover_art.clone())
                .collect();
            match app.later_layout {
                AlbumLayout::List => collect_list_placements(
                    app,
                    main,
                    &covers,
                    app.listen_scroll,
                    app.listen_selected + visible,
                ),
                AlbumLayout::Grid => collect_grid_placements(
                    app,
                    main,
                    &covers,
                    app.listen_scroll,
                    app.listen_selected,
                ),
            }
        }
        View::Playlists | View::PlaylistDetail | View::Search | View::Queue | View::Help => {}
    }

    // Symbol art is painted as cells by ratatui, so the painter must not see
    // it: the requests above are all it needs from this function.
    if !app.protocol.is_pixel() {
        app.placements.clear();
    }
}

/// Place grid thumbnails: a wall draws its covers at their tile rects.
fn collect_grid_placements(
    app: &mut App,
    main: Rect,
    covers: &[Option<String>],
    scroll: usize,
    selected: usize,
) {
    let wall = app.wall(main.width, main.height);
    let per_page = wall.per_page();
    for slot in 0..per_page {
        let index = scroll + slot;
        if index >= covers.len() {
            break;
        }
        let Some(cover) = covers[index].as_deref() else {
            continue;
        };
        let key = app.art.request(
            cover,
            wall.thumb.art_cols,
            wall.thumb.art_rows,
            app.settings.thumb_colors,
            index >= selected + per_page,
        );
        if !app.art.is_ready(&key) {
            continue;
        }
        let (y, x) = wall.cover_offset(slot);
        if y + wall.thumb.rows > main.height {
            break;
        }
        app.placements.push(Placement::new(
            key,
            main.y + y,
            main.x + x,
            wall.thumb.rows,
            wall.thumb.cols,
        ));
    }
}

/// Place list thumbnails for a slice of album covers.
///
/// `prefetch_from` is the first index that counts as lookahead rather than
/// visible work.
fn collect_list_placements(
    app: &mut App,
    main: Rect,
    covers: &[Option<String>],
    scroll: usize,
    prefetch_from: usize,
) {
    let visible = visible_albums(main);
    let thumb = app.list_thumb();
    for slot in 0..visible {
        let index = scroll + slot;
        if index >= covers.len() {
            break;
        }
        let Some(cover) = covers[index].as_deref() else {
            continue;
        };
        let key = app.art.request(
            cover,
            thumb.art_cols,
            thumb.art_rows,
            app.settings.thumb_colors,
            index >= prefetch_from,
        );
        // Only ask for a paint once the blob actually exists: otherwise the
        // painter records the placement as painted and never emits it when the
        // art arrives.
        if !app.art.is_ready(&key) {
            continue;
        }
        app.placements.push(Placement::new(
            key,
            main.y + slot as u16 * ROW_HEIGHT,
            main.x,
            thumb.rows,
            thumb.cols,
        ));
    }
}

pub fn emit_erases(
    terminal: &mut Term,
    plan: &FramePlan,
    protocol: ArtProtocol,
    backdrop: [u8; 3],
    cell: CellSize,
) -> io::Result<()> {
    for op in &plan.erases {
        if let PaintOp::Erase {
            row,
            col,
            rows,
            cols,
            key,
        } = op
        {
            // Cells first: images sit above the character grid, so stale art
            // would still cover whatever text is written underneath.
            let blanks = " ".repeat(usize::from(*cols));
            for offset in 0..*rows {
                queue!(terminal.backend_mut(), MoveTo(*col, row + offset))?;
                terminal.backend_mut().write_all(blanks.as_bytes())?;
            }
            match protocol {
                // No delete under sixel: paint a solid rectangle over it.
                ArtProtocol::Sixel => {
                    // The blank covers the whole slot in real pixels: an image
                    // is drawn at its own pixel size, so a cell-sized blank
                    // would leave a rim of the old cover behind.
                    let (width, height) = cell.pixels_for(*cols, *rows);
                    queue!(terminal.backend_mut(), MoveTo(*col, *row))?;
                    terminal
                        .backend_mut()
                        .write_all(&blank_sixel(width, height, backdrop))?;
                }
                ArtProtocol::Kitty => {
                    if let Some(key) = key {
                        terminal
                            .backend_mut()
                            .write_all(&kitty_delete(kitty_id(key)))?;
                    }
                }
                ArtProtocol::Symbols | ArtProtocol::Off => {}
            }
        }
    }
    terminal.backend_mut().flush()
}

pub fn emit_paints(terminal: &mut Term, plan: &FramePlan, art: &ArtService) -> io::Result<()> {
    for op in &plan.paints {
        if let PaintOp::Paint { placement } = op {
            if let Some(blob) = art.blob(&placement.key) {
                queue!(terminal.backend_mut(), MoveTo(placement.col, placement.row))?;
                terminal.backend_mut().write_all(blob)?;
            }
        }
    }
    terminal.backend_mut().flush()
}

pub fn draw(frame: &mut Frame, app: &App) {
    let area = frame.area();
    // Layout with a rectangle smaller than its constraints would panic.
    if area.width < 20 || area.height < 5 {
        frame.render_widget(
            Paragraph::new("terminal too small").style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }

    let shell = shell_layout(area);
    if let Some(sidebar) = shell.sidebar {
        draw_sidebar(frame, app, sidebar);
    }
    if let Some(divider) = shell.divider {
        for offset in 0..divider.height {
            frame.render_widget(
                Paragraph::new("│").style(Style::default().fg(Color::DarkGray)),
                Rect {
                    y: divider.y + offset,
                    height: 1,
                    ..divider
                },
            );
        }
    }

    draw_view_header(frame, app, shell.header);
    match app.view {
        View::Albums => draw_albums(frame, app, shell.body),
        View::AlbumDetail => draw_album_detail(frame, app, shell.body),
        View::Artists => draw_artists(frame, app, shell.body),
        View::ArtistDetail => draw_artist_detail(frame, app, shell.body),
        View::Favorites => draw_favorites(frame, app, shell.body),
        View::Queue => draw_queue(frame, app, shell.body),
        View::ListenLater => draw_listen_later(frame, app, shell.body),
        View::Playlists => draw_playlists(frame, app, shell.body),
        View::Search => draw_search(frame, app, shell.body),
        View::PlaylistDetail => draw_playlist_detail(frame, app, shell.body),
        View::Help => draw_help(frame, shell.body),
    }

    draw_bar(frame, app, shell.bar);
}

/// The search box, in the header row.
fn draw_search_prompt(frame: &mut Frame, app: &App, area: Rect) {
    let mut spans = vec![
        Span::styled(
            "Search  ",
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(app.search.query.clone(), Style::default().fg(Color::White)),
        Span::styled("▏", Style::default().fg(Color::White)),
    ];
    let used: usize = spans.iter().map(|span| span.width()).sum();
    if used + 24 < usize::from(area.width) {
        spans.push(Span::styled(
            "   Enter to search · Esc to cancel",
            Style::default().fg(MUTED),
        ));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

/// The view's name and sort, on its own row above the list.
///
/// Sorting used to live in a status line shared with playback state, which made
/// it impossible to find and made the list jump when that line appeared.
fn draw_view_header(frame: &mut Frame, app: &App, area: Rect) {
    // The box takes the header over as soon as it opens, on whatever view you
    // were looking at: otherwise you type into an interface that shows nothing.
    if app.search.typing {
        draw_search_prompt(frame, app, area);
        return;
    }

    let (title, sort): (&str, Option<String>) = match app.view {
        View::Albums => ("Albums", Some(app.sort.label().to_string())),
        View::AlbumDetail => ("Album", None),
        View::Artists => ("Artists", Some(app.artist_sort.label().to_string())),
        View::ArtistDetail => ("Artist", None),
        View::Favorites => ("Starred", None),
        View::ListenLater => ("Listen later", None),
        View::Playlists => ("Playlists", None),
        View::PlaylistDetail => ("Playlist", None),
        View::Search => (
            "Search",
            Some(if app.search.typing {
                format!("{}▏", app.search.query)
            } else if app.search.query.is_empty() {
                "press / to search".to_string()
            } else {
                app.search.query.clone()
            }),
        ),
        View::Queue => ("Queue", None),
        View::Help => ("Help", None),
    };
    let counter = match app.view {
        View::Albums => format!("{} / {}", app.selected + 1, app.albums.len()),
        View::Artists => format!("{} / {}", app.artist_selected + 1, app.artists.len()),
        View::Favorites => format!("{}", app.favorites.len()),
        View::ListenLater => format!("{}", app.listen_albums.len()),
        View::Search => format!("{} / {}", app.search.selected + 1, app.search.hits.len()),
        View::Playlists => format!("{}", app.playlists.len()),
        View::PlaylistDetail => app
            .playlist_detail
            .as_ref()
            .map(|playlist| format!("{} / {}", app.playlist_selected + 1, playlist.entry.len()))
            .unwrap_or_default(),
        View::Queue => format!("{}", app.queue.len()),
        View::AlbumDetail => app
            .detail
            .as_ref()
            .map(|album| album.name.clone())
            .unwrap_or_default(),
        View::ArtistDetail => app
            .artist_detail
            .as_ref()
            .map(|artist| artist.name.clone())
            .unwrap_or_default(),
        View::Help => String::new(),
    };

    let counter_w = (counter.chars().count() as u16).min(area.width / 2);
    let mut spans = vec![Span::styled(title, title_style())];
    if let Some(sort) = sort {
        spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        spans.push(Span::styled(sort, Style::default().fg(ACCENT)));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            width: area.width.saturating_sub(counter_w + 1),
            height: 1,
            ..area
        },
    );
    if counter_w > 0 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                counter,
                Style::default().fg(MUTED),
            )))
            .alignment(Alignment::Right),
            Rect {
                x: area.x + area.width - counter_w,
                width: counter_w,
                height: 1,
                ..area
            },
        );
    }
}

fn draw_albums(frame: &mut Frame, app: &App, area: Rect) {
    match app.layout {
        AlbumLayout::List => draw_albums_list(frame, app, area),
        AlbumLayout::Grid => draw_albums_grid(frame, app, area),
    }

    if app.albums.is_empty() {
        frame.render_widget(
            Paragraph::new("No albums. Press R to reload, S to change sort.")
                .style(Style::default().fg(MUTED)),
            area,
        );
    }
}

/// Placeholder shown where a cover has not been painted yet.
fn draw_thumb_placeholder(frame: &mut Frame, rect: Rect, has_cover: bool) {
    if !has_cover {
        return;
    }
    let glyph = "▚ ".repeat(usize::from(rect.width) / 2);
    frame.render_widget(
        Paragraph::new(glyph).style(Style::default().fg(MUTED)),
        rect,
    );
}

fn draw_albums_list(frame: &mut Frame, app: &App, area: Rect) {
    draw_album_listing(
        frame,
        app,
        area,
        &app.albums,
        app.selected,
        app.scroll,
        app.layout,
    );
}

fn draw_listen_later(frame: &mut Frame, app: &App, area: Rect) {
    if app.listen_albums.is_empty() {
        frame.render_widget(
            Paragraph::new(
                "Nothing saved. Press a on an album to keep it for later; L returns here.",
            )
            .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    draw_album_listing(
        frame,
        app,
        area,
        &app.listen_albums,
        app.listen_selected,
        app.listen_scroll,
        app.later_layout,
    );
}

/// Album rows, shared by the library list, listen later and favourites.
/// Albums, as rows or as a wall, whichever the view is set to.
fn draw_album_listing(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    albums: &[crate::subsonic::Album],
    selected: usize,
    scroll: usize,
    layout: AlbumLayout,
) {
    match layout {
        AlbumLayout::List => draw_album_rows(frame, app, area, albums, selected, scroll),
        AlbumLayout::Grid => {
            let tiles: Vec<Tile> = albums
                .iter()
                .map(|album| Tile {
                    cover: album.cover_art.as_deref(),
                    label: &album.name,
                })
                .collect();
            draw_wall(frame, app, area, &tiles, selected, scroll);
        }
    }
}

fn draw_album_rows(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    albums: &[crate::subsonic::Album],
    selected: usize,
    scroll: usize,
) {
    for slot in 0..visible_albums(area) {
        let index = scroll + slot;
        let Some(album) = albums.get(index) else {
            break;
        };
        let year = album
            .year
            .map(|year| format!(" · {year}"))
            .unwrap_or_default();
        let artist = album.artist.clone().unwrap_or_default();
        let tracks = album.song_count.map(|count| format!("{count} tracks"));
        draw_row(
            frame,
            app,
            area,
            slot,
            album.cover_art.as_deref(),
            &album.name,
            &format!("{artist}{year}"),
            tracks.as_deref(),
            index == selected,
            album.starred.is_some(),
        );
    }
}

/// One list row: thumbnail, title, subtitle, and an optional third line.
///
/// Shared by every list view: albums, artists, favourites. Keeping one
/// implementation is what stops the placeholder, the selection marker and the
/// star prefix from drifting apart between screens.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    slot: usize,
    cover: Option<&str>,
    title: &str,
    subtitle: &str,
    detail: Option<&str>,
    selected: bool,
    starred: bool,
) {
    let row = album_row_rect(area, slot);
    let has_art = draw_row_thumb(frame, app, row, cover);

    let gutter = app.list_thumb().cols + 1;
    let text_area = Rect {
        x: row.x + gutter,
        y: row.y,
        width: row.width.saturating_sub(gutter),
        height: ROW_HEIGHT,
    };
    let prefix = if starred { "★ " } else { "" };
    let mut lines = vec![
        Line::from(Span::styled(
            format!("{prefix}{title}"),
            if selected {
                selected_style()
            } else {
                title_style()
            },
        )),
        Line::from(Span::styled(
            subtitle.to_string(),
            Style::default().fg(MUTED),
        )),
    ];
    if let Some(detail) = detail {
        lines.push(Line::from(Span::styled(
            detail.to_string(),
            Style::default().fg(Color::DarkGray),
        )));
    }
    frame.render_widget(Paragraph::new(lines), text_area);

    if selected && !has_art {
        let marker = Rect {
            x: row.x,
            y: row.y,
            width: 1,
            height: 1,
        };
        frame.render_widget(Paragraph::new("▸").style(selected_style()), marker);
    }
}

/// The thumbnail for one row, or its placeholder. Returns whether art is ready.
fn draw_row_thumb(frame: &mut Frame, app: &App, row: Rect, cover: Option<&str>) -> bool {
    let thumb = app.list_thumb();
    let thumb_key = cover.map(|cover| {
        ArtService::key(
            cover,
            thumb.art_cols,
            thumb.art_rows,
            app.settings.thumb_colors,
            app.protocol,
        )
    });
    let has_art = thumb_key
        .as_deref()
        .map(|key| app.art.is_ready(key))
        .unwrap_or(false);
    let slot = Rect {
        x: row.x,
        y: row.y,
        width: thumb.cols,
        height: thumb.rows,
    };
    if has_art {
        if let Some(key) = thumb_key.as_deref() {
            draw_symbol_cover(frame, app, key, slot);
        }
    } else {
        draw_thumb_placeholder(frame, slot, cover.is_some());
    }
    has_art
}

/// Paint a symbol cover into its slot, if the art is ready.
///
/// No-op for pixel protocols: the painter writes those straight to the
/// terminal, and drawing cells over them would erase the image.
fn draw_symbol_cover(frame: &mut Frame, app: &App, key: &str, slot: Rect) {
    if app.protocol.is_pixel() {
        return;
    }
    let Some(blob) = app.art.blob(key) else {
        return;
    };
    let lines = crate::render::symbols::parse(blob, slot.width, slot.height, app.settings.backdrop);
    frame.render_widget(Paragraph::new(lines), slot);
}

fn draw_artists(frame: &mut Frame, app: &App, area: Rect) {
    match app.artist_layout {
        AlbumLayout::Grid => {
            let tiles: Vec<Tile> = app
                .artists
                .iter()
                .map(|artist| Tile {
                    cover: artist.cover_art.as_deref(),
                    label: &artist.name,
                })
                .collect();
            draw_wall(
                frame,
                app,
                area,
                &tiles,
                app.artist_selected,
                app.artist_scroll,
            );
        }
        AlbumLayout::List => {
            let rows = usize::from(area.height / ROW_HEIGHT).max(1);
            for slot in 0..rows {
                let index = app.artist_scroll + slot;
                let Some(artist) = app.artists.get(index) else {
                    break;
                };
                let albums = artist
                    .album_count
                    .map(|count| format!("{count} albums"))
                    .unwrap_or_default();
                draw_row(
                    frame,
                    app,
                    area,
                    slot,
                    artist.cover_art.as_deref(),
                    &artist.name,
                    &albums,
                    None,
                    index == app.artist_selected,
                    artist.starred.is_some(),
                );
            }
        }
    }
}

fn draw_artist_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(artist) = app.artist_detail.as_ref() else {
        return;
    };

    let panes = artist_pane_rects(area);
    draw_artist_left_title(frame, app, panes.albums);
    draw_pane_title(
        frame,
        "similar",
        panes.similar,
        app.artist_pane == ArtistPane::Similar,
    );
    draw_pane_title(
        frame,
        "biography",
        panes.biography,
        app.artist_pane == ArtistPane::Biography,
    );

    // Rules between the panes: the same one-column divider the rail uses, so a
    // narrow pane does not look like text that ran out of room.
    if let Some(rule) = panes.vertical_rule {
        for y in rule.y..rule.y + rule.height {
            frame.render_widget(
                Paragraph::new(Span::styled("│", Style::default().fg(MUTED))),
                Rect {
                    x: rule.x,
                    y,
                    width: 1,
                    height: 1,
                },
            );
        }
    }
    if let Some(rule) = panes.horizontal_rule {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "─".repeat(usize::from(rule.width)),
                Style::default().fg(MUTED),
            )),
            rule,
        );
    }

    let albums_area = panes.albums_body();
    if app.artist_left == ArtistLeft::Songs {
        draw_artist_songs(frame, app, albums_area);
        draw_similar_pane(frame, app, panes.similar_body());
        draw_biography_pane(frame, app, panes.biography_body());
        return;
    }
    for slot in 0..visible_albums(albums_area) {
        let index = app.artist_album_scroll + slot;
        let Some(album) = artist.album.get(index) else {
            break;
        };
        let year = album
            .year
            .map(|year| format!(" · {year}"))
            .unwrap_or_default();
        let tracks = album.song_count.map(|count| format!("{count} tracks"));
        draw_row(
            frame,
            app,
            albums_area,
            slot,
            album.cover_art.as_deref(),
            &album.name,
            &format!("{}{year}", album.artist.clone().unwrap_or_default()),
            tracks.as_deref(),
            index == app.artist_album_selected,
            album.starred.is_some(),
        );
    }

    draw_similar_pane(frame, app, panes.similar_body());
    draw_biography_pane(frame, app, panes.biography_body());
}

/// The left pane's two tabs, side by side; the one in force is bright.
fn draw_artist_left_title(frame: &mut Frame, app: &App, pane: Rect) {
    let focused = app.artist_pane == ArtistPane::Albums;
    let mut spans = Vec::new();
    for tab in [ArtistLeft::Albums, ArtistLeft::Songs] {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ", Style::default().fg(MUTED)));
        }
        let active = app.artist_left == tab;
        let style = match (active, focused) {
            (true, true) => Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
            (true, false) => Style::default().fg(Color::Gray),
            (false, _) => Style::default().fg(MUTED),
        };
        spans.push(Span::styled(tab.label().to_string(), style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)),
        Rect {
            x: pane.x,
            y: pane.y,
            width: pane.width,
            height: 1,
        },
    );
}

/// The artist's top songs: numbered rows, like a playlist.
fn draw_artist_songs(frame: &mut Frame, app: &App, area: Rect) {
    if app.artist_songs.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no top songs for this artist",
                Style::default().fg(MUTED),
            )),
            area,
        );
        return;
    }

    let rows = usize::from(area.height);
    let offset = app
        .artist_song_selected
        .saturating_sub(rows.saturating_sub(1));
    for slot in 0..rows {
        let index = offset + slot;
        let Some(song) = app.artist_songs.get(index) else {
            break;
        };
        let selected = index == app.artist_song_selected
            && app.artist_pane == ArtistPane::Albums
            && app.artist_left == ArtistLeft::Songs;
        let artist = song.album.clone().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(track_line(index + 1, song, &artist, selected, area.width)),
            Rect {
                x: area.x,
                y: area.y + slot as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// Similar artists, as a plain list: they are names, not covers.
fn draw_similar_pane(frame: &mut Frame, app: &App, area: Rect) {
    let Some(info) = app.artist_info.as_ref() else {
        frame.render_widget(
            Paragraph::new(Span::styled("no artist info", Style::default().fg(MUTED))),
            area,
        );
        return;
    };
    if info.similar_artist.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("none known", Style::default().fg(MUTED))),
            area,
        );
        return;
    }

    let rows = usize::from(area.height);
    let offset = app
        .artist_similar_selected
        .saturating_sub(rows.saturating_sub(1));
    for slot in 0..rows {
        let index = offset + slot;
        let Some(artist) = info.similar_artist.get(index) else {
            break;
        };
        let selected =
            index == app.artist_similar_selected && app.artist_pane == ArtistPane::Similar;
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_chars(&artist.name, usize::from(area.width)),
                if selected {
                    selected_style()
                } else {
                    Style::default()
                },
            ))),
            Rect {
                x: area.x,
                y: area.y + slot as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// The biography, wrapped into its column.
fn draw_biography_pane(frame: &mut Frame, app: &App, area: Rect) {
    let text = app
        .artist_info
        .as_ref()
        .and_then(|info| info.biography.as_deref())
        .unwrap_or("");
    if text.trim().is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled("no biography", Style::default().fg(MUTED))),
            area,
        );
        return;
    }

    let width = usize::from(area.width).max(1);
    let lines = wrap(text, width);
    for slot in 0..usize::from(area.height) {
        let index = app.artist_bio_scroll + slot;
        let Some(line) = lines.get(index) else {
            break;
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                line.clone(),
                Style::default().fg(Color::Gray),
            ))),
            Rect {
                x: area.x,
                y: area.y + slot as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// Greedy word wrap, because the biography arrives as one long paragraph and
/// ratatui would otherwise truncate it at the pane edge.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            if line.is_empty() {
                line.push_str(word);
            } else if line.chars().count() + 1 + word.chars().count() <= width {
                line.push(' ');
                line.push_str(word);
            } else {
                lines.push(std::mem::take(&mut line));
                line.push_str(word);
            }
        }
        lines.push(line);
    }
    lines
}

/// Geometry of the artist page: albums on the left, similar artists and the
/// biography stacked on the right.
struct ArtistPanes {
    albums: Rect,
    similar: Rect,
    biography: Rect,
    vertical_rule: Option<Rect>,
    horizontal_rule: Option<Rect>,
}

impl ArtistPanes {
    fn albums_body(&self) -> Rect {
        Rect {
            y: self.albums.y + 1,
            height: self.albums.height.saturating_sub(1),
            ..self.albums
        }
    }

    fn similar_body(&self) -> Rect {
        Rect {
            y: self.similar.y + 1,
            height: self.similar.height.saturating_sub(1),
            ..self.similar
        }
    }

    fn biography_body(&self) -> Rect {
        Rect {
            y: self.biography.y + 1,
            height: self.biography.height.saturating_sub(1),
            ..self.biography
        }
    }
}

fn artist_pane_rects(area: Rect) -> ArtistPanes {
    // Below this the three panes stop being readable; the albums take the
    // width and the rest goes underneath.
    if area.width < 72 {
        let albums_h = (area.height * 6 / 10).max(4);
        return ArtistPanes {
            albums: Rect {
                height: albums_h,
                ..area
            },
            similar: Rect {
                y: area.y + albums_h + 1,
                height: 3,
                ..area
            },
            biography: Rect {
                y: area.y + albums_h + 5,
                height: area.height.saturating_sub(albums_h + 5),
                ..area
            },
            vertical_rule: None,
            horizontal_rule: None,
        };
    }

    let left_w = (area.width * 52 / 100).max(28);
    let rule_x = area.x + left_w;
    let right_x = rule_x + 2;
    let right_w = area.width.saturating_sub(left_w + 2);
    let similar_h = (area.height / 2).max(3);

    ArtistPanes {
        albums: Rect {
            x: area.x,
            width: left_w,
            ..area
        },
        similar: Rect {
            x: right_x,
            y: area.y,
            width: right_w,
            height: similar_h,
        },
        biography: Rect {
            x: right_x,
            y: area.y + similar_h + 1,
            width: right_w,
            height: area.height.saturating_sub(similar_h + 1),
        },
        vertical_rule: Some(Rect {
            x: rule_x,
            y: area.y,
            width: 1,
            height: area.height,
        }),
        horizontal_rule: Some(Rect {
            x: right_x,
            y: area.y + similar_h,
            width: right_w,
            height: 1,
        }),
    }
}

/// A pane's name on its first row; the focused pane's is bright.
fn draw_pane_title(frame: &mut Frame, label: &str, pane: Rect, focused: bool) {
    let style = if focused {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(MUTED)
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(label.to_string(), style))),
        Rect {
            x: pane.x,
            y: pane.y,
            width: pane.width.min(label.chars().count() as u16 + 1),
            height: 1,
        },
    );
}

fn draw_favorites(frame: &mut Frame, app: &App, area: Rect) {
    if app.favorites.is_empty() {
        frame.render_widget(
            Paragraph::new("Nothing starred yet. Press * on an album to keep it here.")
                .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    draw_album_listing(
        frame,
        app,
        area,
        &app.favorites,
        app.favorite_selected,
        app.favorite_scroll,
        app.favorites_layout,
    );
}

/// One tile of a wall: a cover and the line of text under it.
struct Tile<'a> {
    cover: Option<&'a str>,
    label: &'a str,
}

/// A wall of covers. Shared by the album list and the artist list.
fn draw_wall(
    frame: &mut Frame,
    app: &App,
    area: Rect,
    tiles: &[Tile],
    selected: usize,
    scroll: usize,
) {
    let wall = app.wall(area.width, area.height);
    let size = wall.thumb;

    for slot in 0..wall.per_page() {
        let index = scroll + slot;
        let Some(tile) = tiles.get(index) else {
            break;
        };
        let is_selected = index == selected;
        if wall.offset(slot).0 + wall.thumb.rows > area.height {
            break;
        }
        let (dy, dx) = wall.cover_offset(slot);
        let x = area.x + dx;
        let y = area.y + dy;

        let thumb_key = tile.cover.map(|cover| {
            ArtService::key(
                cover,
                size.art_cols,
                size.art_rows,
                app.settings.thumb_colors,
                app.protocol,
            )
        });
        let has_art = thumb_key
            .as_deref()
            .map(|key| app.art.is_ready(key))
            .unwrap_or(false);
        let thumb = Rect {
            x,
            y,
            width: size.cols,
            height: size.rows,
        };
        if has_art {
            if let Some(key) = thumb_key.as_deref() {
                draw_symbol_cover(frame, app, key, thumb);
            }
        } else {
            draw_thumb_placeholder(frame, thumb, tile.cover.is_some());
        }

        // One line of text under the cover; the tile is too narrow for more.
        let label = if is_selected {
            format!("\u{25b8}{}", tile.label)
        } else {
            format!(" {}", tile.label)
        };
        frame.render_widget(
            Paragraph::new(label).style(if is_selected {
                selected_style()
            } else {
                Style::default().fg(MUTED)
            }),
            Rect {
                x: area.x + wall.offset(slot).1,
                y: y + size.rows,
                width: wall.label_w(),
                height: 1,
            },
        );
    }
}

fn draw_albums_grid(frame: &mut Frame, app: &App, area: Rect) {
    let tiles: Vec<Tile> = app
        .albums
        .iter()
        .map(|album| Tile {
            cover: album.cover_art.as_deref(),
            label: &album.name,
        })
        .collect();
    draw_wall(frame, app, area, &tiles, app.selected, app.scroll);
}

fn draw_album_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(album) = app.detail.as_ref() else {
        frame.render_widget(Paragraph::new("No album"), area);
        return;
    };

    let header = Rect {
        height: area.height.min(COVER_SPACE_H),
        ..area
    };
    let body = Rect {
        y: area.y + header.height,
        height: area.height.saturating_sub(header.height),
        ..area
    };

    // The cover is sized the same way as everywhere else, so the information
    // sits beside whatever width the square actually took.
    let cover = app.square_thumb(COVER_W, header.height);
    let info_x = area.x + cover.cols + 1;
    let info_width = area.width.saturating_sub(cover.cols + 2);
    let info = Rect {
        x: info_x,
        y: area.y,
        width: info_width,
        height: header.height,
    };

    let year = album
        .year
        .map(|y| y.to_string())
        .unwrap_or_else(|| "—".to_string());
    let lines = vec![
        Line::from(Span::styled(album.name.clone(), title_style())),
        Line::from(Span::styled(
            album.artist.clone().unwrap_or_default(),
            Style::default().fg(ACCENT),
        )),
        Line::from(Span::styled(
            format!("{year} · {} tracks", album.song.len()),
            Style::default().fg(MUTED),
        )),
    ];
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: true }), info);

    // Track list.
    let rows = body.height as usize;
    let offset = app.detail_selected.saturating_sub(rows.saturating_sub(1));
    for slot in 0..rows {
        let index = offset + slot;
        let Some(song) = album.song.get(index) else {
            break;
        };
        let selected = index == app.detail_selected;
        let duration = song
            .duration
            .map(format_duration)
            .unwrap_or_else(|| "--:--".to_string());
        let line = Line::from(vec![
            Span::styled(format!("{:>3}. ", index + 1), Style::default().fg(MUTED)),
            Span::styled(
                song.title.clone(),
                if selected {
                    selected_style()
                } else {
                    Style::default()
                },
            ),
            Span::styled(format!("  {duration}"), Style::default().fg(MUTED)),
        ]);
        let row = Rect {
            x: body.x,
            y: body.y + slot as u16,
            width: body.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(line), row);
    }
}

/// Search results: albums first, then songs, each row saying which it is.
fn draw_search(frame: &mut Frame, app: &App, area: Rect) {
    if app.search.hits.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                if app.search.typing {
                    "type a query, Enter to search"
                } else {
                    "no results"
                },
                Style::default().fg(MUTED),
            )),
            area,
        );
        return;
    }

    let rows = usize::from(area.height);
    let offset = app.search.selected.saturating_sub(rows.saturating_sub(1));
    for slot in 0..rows {
        let index = offset + slot;
        let Some(hit) = app.search.hits.get(index) else {
            break;
        };
        let selected = index == app.search.selected;
        let (kind, name, detail) = match hit {
            crate::app::SearchHit::Album(album) => (
                "album",
                album.name.clone(),
                album.artist.clone().unwrap_or_default(),
            ),
            crate::app::SearchHit::Artist(artist) => (
                "artist",
                artist.name.clone(),
                artist
                    .album_count
                    .map(|count| format!("{count} albums"))
                    .unwrap_or_default(),
            ),
            crate::app::SearchHit::Song(song) => (
                "song",
                song.title.clone(),
                song.artist.clone().unwrap_or_default(),
            ),
        };
        let tail_width = usize::from(area.width)
            .saturating_sub(usize::from(area.width) / 2 + 9)
            .max(8);
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(format!("{kind:<6}"), Style::default().fg(MUTED)),
                Span::styled(
                    truncate_chars(&name, usize::from(area.width).saturating_sub(20).max(8)),
                    if selected {
                        selected_style()
                    } else {
                        Style::default()
                    },
                ),
                Span::styled(
                    format!("  {}", truncate_chars(&detail, tail_width)),
                    Style::default().fg(MUTED),
                ),
            ])),
            Rect {
                x: area.x,
                y: area.y + slot as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// The playlist list: a name and how much is in it.
fn draw_playlists(frame: &mut Frame, app: &App, area: Rect) {
    if app.playlists.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "no playlists on this server",
                Style::default().fg(MUTED),
            )),
            area,
        );
        return;
    }

    let rows = usize::from(area.height);
    let offset = app.playlist_selected.saturating_sub(rows.saturating_sub(1));
    for slot in 0..rows {
        let index = offset + slot;
        let Some(playlist) = app.playlists.get(index) else {
            break;
        };
        let selected = index == app.playlist_selected;
        let count = match playlist.song_count {
            Some(count) => format!("{count} tracks"),
            None => String::new(),
        };
        let name_width = usize::from(area.width)
            .saturating_sub(count.chars().count() + 4)
            .max(8);
        let line = Line::from(vec![
            Span::styled(
                truncate_chars(&playlist.name, name_width),
                if selected {
                    selected_style()
                } else {
                    Style::default()
                },
            ),
            Span::styled(format!("  {count}"), Style::default().fg(MUTED)),
        ]);
        frame.render_widget(
            Paragraph::new(line),
            Rect {
                x: area.x,
                y: area.y + slot as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// One playlist's tracks.
fn draw_playlist_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(playlist) = app.playlist_detail.as_ref() else {
        frame.render_widget(Paragraph::new("No playlist"), area);
        return;
    };
    if playlist.entry.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                "this playlist is empty",
                Style::default().fg(MUTED),
            )),
            area,
        );
        return;
    }

    let rows = usize::from(area.height);
    let offset = app.playlist_selected.saturating_sub(rows.saturating_sub(1));
    for slot in 0..rows {
        let index = offset + slot;
        let Some(song) = playlist.entry.get(index) else {
            break;
        };
        let selected = index == app.playlist_selected;
        let artist = song.artist.clone().unwrap_or_default();
        frame.render_widget(
            Paragraph::new(track_line(index + 1, song, &artist, selected, area.width)),
            Rect {
                x: area.x,
                y: area.y + slot as u16,
                width: area.width,
                height: 1,
            },
        );
    }
}

/// One numbered track row, shared by the playlist view.
fn track_line<'a>(
    number: usize,
    song: &crate::subsonic::Song,
    artist: &str,
    selected: bool,
    width: u16,
) -> Line<'a> {
    let duration = song
        .duration
        .map(format_duration)
        .unwrap_or_else(|| "--:--".to_string());
    let tail = format!("{artist}  {duration}").trim().to_string();
    let title_width = usize::from(width)
        .saturating_sub(tail.chars().count() + 8)
        .max(8);
    Line::from(vec![
        Span::styled(format!("{number:>3}. "), Style::default().fg(MUTED)),
        Span::styled(
            truncate_chars(&song.title, title_width),
            if selected {
                selected_style()
            } else {
                Style::default()
            },
        ),
        Span::styled(format!("  {tail}"), Style::default().fg(MUTED)),
    ])
}

fn draw_queue(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::ALL).title(format!(
        " Queue — {} tracks · shuffle {} · repeat {} ",
        app.queue.len(),
        if app.queue.is_shuffled() { "on" } else { "off" },
        app.queue.repeat().label()
    ));
    let inner = block.inner(area);
    frame.render_widget(block, area);

    let items = app.queue.playback_items();
    let playing = app.queue.position();
    let selected = app.queue_cursor();
    let rows = inner.height as usize;
    let offset = selected.saturating_sub(rows.saturating_sub(1));

    for slot in 0..rows {
        let index = offset + slot;
        let Some(item) = items.get(index) else {
            break;
        };
        let is_playing = Some(index) == playing;
        let is_selected = index == selected;
        // Two different questions, two different marks: `▶` says this is what
        // you hear, `›` says this is where Enter goes. A playing row that is
        // also selected keeps `▶` and takes the selected colour.
        let marker = if is_playing {
            "▶ "
        } else if is_selected {
            "› "
        } else {
            "  "
        };
        let title = if is_selected {
            selected_style()
        } else if is_playing {
            playing_style()
        } else {
            Style::default()
        };
        let line = Line::from(vec![
            Span::styled(
                marker,
                if is_selected || is_playing {
                    Style::default().fg(ACCENT)
                } else {
                    Style::default()
                },
            ),
            Span::styled(item.title.clone(), title),
            Span::styled(
                format!(" — {}", item.artist.clone().unwrap_or_default()),
                Style::default().fg(MUTED),
            ),
        ]);
        let row = Rect {
            x: inner.x,
            y: inner.y + slot as u16,
            width: inner.width,
            height: 1,
        };
        frame.render_widget(Paragraph::new(line), row);
    }
}

fn draw_help(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(Span::styled("Undertone", title_style())),
        Line::from(""),
        Line::from("j / k      move down / up"),
        Line::from("Ctrl-d/u   half-page down / up"),
        Line::from("h / l      move left / right (wall) · h back"),
        Line::from("w          toggle list / wall"),
        Line::from("g / G      first / last"),
        Line::from("Enter      open album · play track"),
        Line::from("p / Space  play / pause"),
        Line::from("> / <      next / previous track"),
        Line::from("f / b      seek forward / back 10s"),
        Line::from("s / r      shuffle / repeat"),
        Line::from("+ / -      volume"),
        Line::from("m          mute"),
        Line::from("Q          queue"),
        Line::from("L          listen later      A  artists      F  favourites"),
        Line::from("a          save / unsave album         *  star / unstar"),
        Line::from("S / R      change sort / reload"),
        Line::from("Esc        back"),
        Line::from("q          quit"),
        Line::from(""),
        Line::from(Span::styled(
            "Any key closes this screen.",
            Style::default().fg(MUTED),
        )),
    ];
    let width = 44.min(area.width);
    let height = (lines.len() as u16 + 2).min(area.height);
    let popup = Rect {
        x: area.x + (area.width.saturating_sub(width)) / 2,
        y: area.y + (area.height.saturating_sub(height)) / 2,
        width,
        height,
    };
    frame.render_widget(Clear, popup);
    let block = Block::default().borders(Borders::ALL).title(" Help ");
    frame.render_widget(Paragraph::new(lines).block(block), popup);
}

/// The bottom bar: playing track, state and keys, then the queue.
///
/// Three equal columns. Everything here is about the next few seconds, which is
/// why it is allowed to take five rows on a screen where every row costs.
fn draw_bar(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 30 || inner.height < 2 {
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Length(1),
            Constraint::Ratio(1, 3),
            Constraint::Length(1),
            Constraint::Ratio(1, 3),
        ])
        .split(inner);
    for rule in [columns[1], columns[3]] {
        for offset in 0..rule.height {
            frame.render_widget(
                Paragraph::new("│").style(Style::default().fg(Color::DarkGray)),
                Rect {
                    y: rule.y + offset,
                    height: 1,
                    ..rule
                },
            );
        }
    }

    draw_bar_track(frame, app, columns[0]);
    draw_bar_state(frame, app, columns[2]);
    draw_bar_queue(frame, app, columns[4]);
}

fn draw_bar_track(frame: &mut Frame, app: &App, area: Rect) {
    let row = |offset: u16| Rect {
        y: area.y + offset,
        height: 1,
        ..area
    };

    let Some(track) = app.now.track.as_ref() else {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "nothing playing",
                Style::default().fg(MUTED),
            ))),
            row(0),
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "Enter on an album to start",
                Style::default().fg(Color::DarkGray),
            ))),
            row(1),
        );
        return;
    };

    let artist = track.artist.clone().unwrap_or_default();
    let mut title = String::new();
    if !artist.is_empty() {
        title.push_str(&artist);
        title.push_str(" — ");
    }
    title.push_str(&track.title);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                if app.now.paused { "❚❚ " } else { "▶ " },
                Style::default().fg(ACCENT),
            ),
            Span::styled(
                // Three columns for the marker, so the ellipsis lands inside
                // the section rather than being clipped by it.
                truncate_chars(&title, usize::from(area.width).saturating_sub(3)),
                title_style(),
            ),
        ])),
        row(0),
    );

    // Timestamp, progress, timestamp: the whole remaining width is the bar.
    let clock = |secs: f64| format_duration(secs.max(0.0).round() as u32);
    let left = clock(app.now.position);
    let right = clock(app.now.duration);
    let left_w = left.chars().count() as u16;
    let right_w = right.chars().count() as u16;
    let bar_x = area.x + left_w + 1;
    let bar_w = area.width.saturating_sub(left_w + right_w + 2);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(left, Style::default().fg(MUTED)))),
        row(1),
    );
    draw_thin_progress(
        frame,
        app,
        Rect {
            x: bar_x,
            y: area.y + 1,
            width: bar_w,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(right, Style::default().fg(MUTED))))
            .alignment(Alignment::Right),
        Rect {
            x: area.x + area.width - right_w,
            width: right_w,
            height: 1,
            ..row(1)
        },
    );

    // Album and container/bitrate: asked for early on, and it needs a home
    // here now that the bar carries no shared status line.
    let mut meta = String::new();
    if let Some(album) = track.album.as_deref().filter(|album| !album.is_empty()) {
        meta.push_str(album);
    }
    if let Some(format) = track.format_label() {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&format);
    }
    if area.height > 2 && !meta.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_chars(&meta, usize::from(area.width)),
                Style::default().fg(MUTED),
            ))),
            row(2),
        );
    }
}

/// Playback state, plus whatever the last action said, plus the keys.
fn draw_bar_state(frame: &mut Frame, app: &App, area: Rect) {
    let row = |offset: u16| Rect {
        y: area.y + offset,
        height: 1,
        ..area
    };

    let fresh = app
        .status
        .as_ref()
        .filter(|status| status.at.elapsed() < crate::app::STATUS_TTL);
    match fresh {
        Some(status) => frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_chars(&status.text, usize::from(area.width)),
                if status.error {
                    Style::default().fg(Color::Red)
                } else {
                    Style::default().fg(ACCENT)
                },
            ))),
            row(0),
        ),
        None => frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(
                    "repeat {} · shuffle {}",
                    app.queue.repeat().label(),
                    if app.queue.is_shuffled() { "on" } else { "off" }
                ),
                Style::default().fg(MUTED),
            ))),
            row(0),
        ),
    }

    let hints = key_hints(app.view);
    if area.height > 1 && !hints.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                hints,
                Style::default().fg(Color::DarkGray),
            )))
            .wrap(Wrap { trim: true }),
            Rect {
                y: area.y + 1,
                height: area.height - 1,
                ..area
            },
        );
    }
}

fn draw_bar_queue(frame: &mut Frame, app: &App, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "next",
            Style::default().fg(Color::DarkGray),
        ))),
        Rect {
            y: area.y,
            height: 1,
            ..area
        },
    );

    let upcoming = app
        .queue
        .upcoming(usize::from(area.height.saturating_sub(1)));
    if upcoming.is_empty() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "end of queue",
                Style::default().fg(Color::DarkGray),
            ))),
            Rect {
                y: area.y + 1,
                height: 1,
                ..area
            },
        );
        return;
    }
    for (index, item) in upcoming.iter().enumerate() {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_chars(&item.title, usize::from(area.width)),
                Style::default().fg(MUTED),
            ))),
            Rect {
                y: area.y + 1 + index as u16,
                height: 1,
                ..area
            },
        );
    }
}

/// Keys worth knowing on this screen, shown under the playback state.
fn key_hints(view: View) -> &'static str {
    match view {
        View::Albums => "S sort · w layout · Enter · * star · ? help",
        View::AlbumDetail => "Enter play · * star · A artist · h back",
        View::Artists => "S sort · w layout · Enter open · h back",
        View::ArtistDetail => "Tab pane · t songs · Enter open · h back",
        View::Favorites => "w layout · Enter open · * unstar · h back",
        View::ListenLater => "w layout · a remove · Enter open · h back",
        View::Playlists => "Enter open · h back",
        View::Search => "/ edit · Enter open · h back",
        View::PlaylistDetail => "Enter play · h back",
        View::Queue => "Enter play · > skip · h back",
        View::Help => "",
    }
}

/// A one-row progress bar: heavy rule for played, light rule for the rest.
fn draw_thin_progress(frame: &mut Frame, app: &App, area: Rect) {
    if area.width == 0 {
        return;
    }
    let ratio = if app.now.duration > 0.0 {
        (app.now.position / app.now.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let filled = (f64::from(area.width) * ratio).round() as u16;
    let line = Line::from(vec![
        Span::styled("━".repeat(usize::from(filled)), Style::default().fg(ACCENT)),
        Span::styled(
            "─".repeat(usize::from(area.width - filled)),
            Style::default().fg(MUTED),
        ),
    ]);
    frame.render_widget(Paragraph::new(line), area);
}

/// Clamp to `width` characters, with an ellipsis when it had to cut.
fn truncate_chars(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

pub fn format_duration(seconds: u32) -> String {
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Thumb;

    #[test]
    fn durations_format_as_minutes_and_seconds() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(9), "0:09");
        assert_eq!(format_duration(65), "1:05");
        assert_eq!(format_duration(600), "10:00");
        assert_eq!(format_duration(3599), "59:59");
    }

    #[test]
    fn layout_gives_every_view_a_header_and_a_bar() {
        let area = Rect::new(0, 0, 80, 24);
        let shell = shell_layout(area);

        // The bar is always there: it carries state, keys and the queue.
        assert_eq!(shell.bar.height, BAR_HEIGHT);
        assert_eq!(shell.bar.y, 24 - BAR_HEIGHT);

        // Every view gets one row for its title and sort.
        assert_eq!(shell.header.height, HEADER_HEIGHT);
        assert_eq!(shell.body.y, shell.header.y + HEADER_HEIGHT);
        assert!(shell.body.height > 0);

        // The rail sits to the left of the content, with a rule between them.
        let sidebar = shell.sidebar.expect("rail");
        assert_eq!(sidebar.width, SIDEBAR_W);
        assert_eq!(sidebar.x, area.x);
        let divider = shell.divider.expect("divider");
        assert_eq!(divider.x, sidebar.x + SIDEBAR_W);
        assert_eq!(divider.height, shell.header.height + shell.body.height);
        assert!(shell.body.x > divider.x, "the view never touches the rail");
    }

    // ---------------------------------------------------------------- art

    use crate::config::Settings;
    use crate::player::NullPlayer;
    use crate::render::ArtProtocol;
    use crate::subsonic::{Album, SubsonicClient, SubsonicConfig};
    use std::path::PathBuf;

    fn test_app(protocol: ArtProtocol) -> App {
        let client = SubsonicClient::new(SubsonicConfig {
            server: "https://demo.navidrome.org".into(),
            username: "demo".into(),
            password: "demo".into(),
        });
        let settings = Settings {
            server: "https://demo.navidrome.org".into(),
            username: "demo".into(),
            password: "demo".into(),
            scrobble: false,
            auto_dj: false,
            art_mode: "sixel".into(),
            thumb_colors: 16,
            cover_colors: 240,
            cache_dir: PathBuf::from("/tmp/undertone-ui-test"),
            cache_bytes: 1024 * 1024,
            mpv_binary: PathBuf::from("mpv"),
            transcode: false,
            backdrop: [0, 0, 0],
        };
        // No image source: placements are computed but no blobs ever arrive,
        // which is exactly what we want to test layout, not rendering.
        let art = crate::art::ArtService::from_settings(
            protocol,
            &settings.cache_dir,
            settings.cache_bytes,
            None,
        )
        .expect("art service");
        App::new(
            client,
            settings,
            Box::new(NullPlayer::new()),
            art,
            protocol,
            // A plain 8x17 cell: the fallback, and what most desktops report.
            crate::render::CellSize {
                width: 8,
                height: 17,
            },
            70,
            fresh_listen_later(),
        )
    }

    fn fresh_listen_later() -> crate::listen_later::ListenLater {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "undertone-ui-ll-{}-{}.json",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::remove_file(&path);
        crate::listen_later::ListenLater::load(path)
    }

    fn album_with_cover(id: &str) -> Album {
        Album {
            id: id.to_string(),
            name: format!("album {id}"),
            cover_art: Some(format!("al-{id}_cover")),
            ..Default::default()
        }
    }

    /// The thumbnail a wall draws in this area, so a test can warm the blobs
    /// at the size the layout will actually ask for.
    fn wall_thumb(app: &App, area: Rect) -> Thumb {
        let body = shell_layout(area).body;
        app.wall(body.width, body.height).thumb
    }

    /// Pretend every album's thumbnail has finished rendering.
    fn warm_art(app: &mut App, cols: u32, rows: u32) {
        for album in &app.albums {
            let Some(cover) = album.cover_art.as_deref() else {
                continue;
            };
            let key = ArtService::key(cover, cols, rows, 16, ArtProtocol::Sixel);
            app.art.force_blob(&key, b"\x1bPfake".to_vec());
        }
    }

    /// Render one frame into an offscreen buffer and return its text.
    ///
    /// Placements are exercised elsewhere; this is for what the user reads.
    fn rendered_text(app: &App, width: u16, height: u16) -> String {
        use ratatui::backend::TestBackend;
        let backend = TestBackend::new(width, height);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| draw(frame, app)).expect("draw");
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..height {
            for x in 0..width {
                // Index rather than iterate: a wide character leaves an empty
                // continuation cell, and concatenating those silently shifts
                // every later row.
                let symbol = buffer[(x, y)].symbol();
                text.push_str(if symbol.is_empty() { " " } else { symbol });
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn switching_views_erases_previously_painted_art() {
        let mut app = test_app(ArtProtocol::Sixel);
        app.albums = vec![album_with_cover("a"), album_with_cover("b")];
        let thumb = app.list_thumb();
        warm_art(&mut app, thumb.art_cols, thumb.art_rows);
        let area = Rect::new(0, 0, 100, 30);

        collect_placements(&mut app, area);
        let first = app.painter.plan(&app.placements);
        assert_eq!(first.paints.len(), 2);
        assert!(first.erases.is_empty());

        // Opening the queue draws no art, so the two covers underneath must be
        // erased. Forgetting them (invalidate) leaves sixel pixels on screen.
        app.view = View::Queue;
        collect_placements(&mut app, area);
        let second = app.painter.plan(&app.placements);
        assert_eq!(second.erases.len(), 2, "stale covers must be erased");
        assert!(second.paints.is_empty());
    }

    #[test]
    fn going_back_to_albums_repaints_the_thumbnails() {
        let mut app = test_app(ArtProtocol::Sixel);
        app.albums = vec![album_with_cover("a")];
        let thumb = app.list_thumb();
        warm_art(&mut app, thumb.art_cols, thumb.art_rows);
        let area = Rect::new(0, 0, 100, 30);

        collect_placements(&mut app, area);
        app.painter.plan(&app.placements);

        app.view = View::Queue;
        collect_placements(&mut app, area);
        app.painter.plan(&app.placements);

        app.view = View::Albums;
        collect_placements(&mut app, area);
        let plan = app.painter.plan(&app.placements);
        assert_eq!(plan.paints.len(), 1, "returning must repaint the cover");
    }

    #[test]
    fn grid_placements_tile_without_overlapping() {
        let mut app = test_app(ArtProtocol::Sixel);
        app.layout = AlbumLayout::Grid;
        app.albums = (0..12).map(|i| album_with_cover(&i.to_string())).collect();
        let area = Rect::new(0, 0, 100, 30);
        let thumb = wall_thumb(&app, area);
        warm_art(&mut app, thumb.art_cols, thumb.art_rows);

        collect_placements(&mut app, area);
        assert_eq!(app.placements.len(), 12);
        for a in &app.placements {
            assert!(a.col + a.cols <= area.width, "placement must fit the width");
            assert!(
                a.row + a.rows <= area.height,
                "placement must fit the height"
            );
        }
        for (i, a) in app.placements.iter().enumerate() {
            for b in app.placements.iter().skip(i + 1) {
                assert!(!a.intersects(b), "{a:?} overlaps {b:?}");
            }
        }
    }

    #[test]
    fn the_wall_uses_the_whole_body_of_a_small_screen() {
        // The device this was tuned on: 115x25 cells of 16x41 pixels. The body
        // is 101x19, which used to hold two rows of covers and a blank band.
        let mut app = test_app(ArtProtocol::Sixel);
        app.cell = CellSize {
            width: 16,
            height: 41,
        };
        app.layout = AlbumLayout::Grid;
        app.albums = (0..24).map(|i| album_with_cover(&i.to_string())).collect();
        let area = Rect::new(0, 0, 115, 25);
        let thumb = wall_thumb(&app, area);
        warm_art(&mut app, thumb.art_cols, thumb.art_rows);

        collect_placements(&mut app, area);
        assert_eq!(app.placements.len(), 21, "three rows of seven covers");

        let body = shell_layout(area).body;
        for placement in &app.placements {
            assert!(
                placement.row + placement.rows <= body.y + body.height,
                "{placement:?} runs past the body"
            );
            assert!(placement.col + placement.cols <= body.x + body.width);
        }
        let lowest = app
            .placements
            .iter()
            .map(|placement| placement.row + placement.rows)
            .max()
            .expect("placements");
        assert!(
            body.y + body.height - lowest < thumb.rows,
            "the last row of covers reaches the bottom of the body"
        );
    }

    #[test]
    fn a_placement_asks_for_the_art_it_paints() {
        // The placement's cell rectangle and the size it was rendered for have
        // to agree: the painter reserves the first and the blob fills it.
        let mut app = test_app(ArtProtocol::Sixel);
        app.cell = CellSize {
            width: 16,
            height: 41,
        };
        let thumb = app.list_thumb();
        app.albums = vec![album_with_cover("a")];
        warm_art(&mut app, thumb.art_cols, thumb.art_rows);

        collect_placements(&mut app, Rect::new(0, 0, 115, 25));
        let placement = app.placements.first().expect("one cover");
        assert_eq!((placement.cols, placement.rows), (thumb.cols, thumb.rows));
        assert_eq!(
            placement.key,
            ArtService::key(
                "al-a_cover",
                thumb.art_cols,
                thumb.art_rows,
                16,
                ArtProtocol::Sixel
            ),
            "the key names the size draw_row_thumb looks for"
        );
    }

    #[test]
    fn symbols_mode_places_no_pixels_at_all() {
        let mut app = test_app(ArtProtocol::Symbols);
        app.albums = vec![album_with_cover("a")];
        collect_placements(&mut app, Rect::new(0, 0, 100, 30));
        assert!(app.placements.is_empty());
        assert!(app.painter.plan(&app.placements).is_empty());
    }

    fn playing_app() -> App {
        let mut app = test_app(ArtProtocol::Off);
        app.now.track = Some(crate::queue::QueueItem {
            id: "s1".into(),
            title: "Everything In Its Right Place".into(),
            artist: Some("Radiohead".into()),
            album: Some("Kid A".into()),
            duration: Some(324),
            suffix: Some("flac".into()),
            bit_rate: Some(1044),
            ..Default::default()
        });
        app.now.position = 73.0;
        app.now.duration = 324.0;
        app.now.loaded = true;
        app.now.paused = false;
        app.queue.replace(vec![
            crate::queue::QueueItem {
                id: "s2".into(),
                title: "Kid A".into(),
                ..Default::default()
            },
            crate::queue::QueueItem {
                id: "s3".into(),
                title: "The National Anthem".into(),
                ..Default::default()
            },
        ]);
        app
    }

    #[test]
    fn the_bar_shows_the_track_progress_and_both_timestamps() {
        let app = playing_app();
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Radiohead — Everything"), "{text}");
        assert!(text.contains("1:13"), "elapsed time: {text}");
        assert!(text.contains("5:24"), "total time: {text}");
        // Album and container: the bar is the only place they live.
        assert!(text.contains("Kid A · FLAC 1044 kbps"), "{text}");
    }

    #[test]
    fn the_bar_lists_what_plays_next() {
        let app = playing_app();
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("next"), "{text}");
        assert!(text.contains("The National Anthem"), "{text}");
    }

    #[test]
    fn the_queue_marks_playing_and_selected_differently() {
        let mut app = playing_app(); // s2 and s3 are queued, s2 plays
        app.view = View::Queue;
        app.queue.jump_to(0);
        app.queue_pin = Some(1);

        let text = rendered_text(&app, 100, 24);

        assert!(
            text.contains("▶ Kid A"),
            "the track that is playing keeps its play mark: {text}"
        );
        assert!(
            text.contains("› The National Anthem"),
            "the cursor has a mark of its own: {text}"
        );
        assert!(
            text.contains("next") && text.contains("The National Anthem"),
            "what plays next follows the playing row, not the cursor: {text}"
        );
    }

    #[test]
    fn the_bar_shows_repeat_and_shuffle_state() {
        let app = test_app(ArtProtocol::Off);
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("repeat off"), "{text}");
        assert!(text.contains("shuffle off"), "{text}");
    }

    #[test]
    fn the_header_carries_the_view_name_and_its_sort() {
        let mut app = test_app(ArtProtocol::Off);
        app.view = View::Albums;
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Albums · random"), "{text}");

        app.view = View::Artists;
        app.artist_sort = crate::app::ArtistSort::Name;
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Artists · a-z"), "{text}");
    }

    #[test]
    fn status_messages_take_over_the_state_line_and_expire() {
        let mut app = test_app(ArtProtocol::Off);
        app.status("starred an album");
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("starred an album"), "{text}");
        assert!(
            !text.contains("repeat off"),
            "the message replaces the state"
        );

        // Past the TTL the state comes back.
        app.status = Some(crate::app::Status {
            text: "starred an album".into(),
            at: std::time::Instant::now() - crate::app::STATUS_TTL * 2,
            error: false,
        });
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("repeat off"), "{text}");
    }

    fn playlist(name: &str, tracks: usize) -> crate::subsonic::Playlist {
        crate::subsonic::Playlist {
            id: format!("pl-{name}"),
            name: name.to_string(),
            song_count: Some(tracks as u32),
            entry: (0..tracks)
                .map(|i| crate::subsonic::Song {
                    id: format!("{name}-{i}"),
                    title: format!("song {i}"),
                    artist: Some("someone".into()),
                    duration: Some(200),
                    ..Default::default()
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_search_box_takes_over_the_header_while_typing() {
        let mut app = test_app(ArtProtocol::Off);
        app.search.typing = true;
        app.search.query = "radio".into();

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Search  radio▏"), "{text}");
        assert!(text.contains("Enter to search"), "{text}");
        // The album header is not drawn underneath it.
        assert!(!text.contains("Albums · random"), "{text}");
    }

    #[test]
    fn search_rows_name_artists_as_well_as_albums_and_songs() {
        let mut app = test_app(ArtProtocol::Off);
        app.view = View::Search;
        app.search.query = "radio".into();
        app.search.hits = vec![
            crate::app::SearchHit::Album(album_with_cover("al1")),
            crate::app::SearchHit::Artist(crate::subsonic::Artist {
                id: "ar1".into(),
                name: "Radiohead".into(),
                album_count: Some(9),
                ..Default::default()
            }),
            crate::app::SearchHit::Song(crate::subsonic::Song {
                id: "s1".into(),
                title: "Idioteque".into(),
                ..Default::default()
            }),
        ];

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("album"), "{text}");
        assert!(text.contains("artist"), "{text}");
        assert!(text.contains("Radiohead"), "{text}");
        assert!(text.contains("9 albums"), "{text}");
        assert!(text.contains("1 / 3"), "{text}");
    }

    #[test]
    fn search_results_say_which_kind_each_row_is() {
        let mut app = test_app(ArtProtocol::Off);
        app.view = View::Search;
        app.search.query = "kid".into();
        app.search.hits = vec![
            crate::app::SearchHit::Album(album_with_cover("al1")),
            crate::app::SearchHit::Song(crate::subsonic::Song {
                id: "s1".into(),
                title: "Kid A".into(),
                artist: Some("Radiohead".into()),
                ..Default::default()
            }),
        ];

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("album"), "{text}");
        assert!(text.contains("song"), "{text}");
        assert!(text.contains("Radiohead"), "{text}");
        assert!(text.contains("1 / 2"), "{text}");
    }

    #[test]
    fn playlists_are_listed_with_their_size() {
        let mut app = test_app(ArtProtocol::Off);
        app.playlists = vec![playlist("morning", 12), playlist("focus", 4)];
        app.view = View::Playlists;

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("morning"), "{text}");
        assert!(text.contains("12 tracks"), "{text}");
        assert!(text.contains("focus"), "{text}");
        assert!(
            text.contains("Playlists"),
            "the header names the view: {text}"
        );
    }

    #[test]
    fn a_playlist_shows_its_tracks_numbered() {
        let mut app = test_app(ArtProtocol::Off);
        app.playlist_detail = Some(playlist("morning", 3));
        app.view = View::PlaylistDetail;

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("song 0"), "{text}");
        assert!(text.contains("song 2"), "{text}");
        assert!(
            text.contains("1 / 3"),
            "the counter follows the cursor: {text}"
        );
    }

    #[test]
    fn the_rail_lists_playlists_and_keeps_the_queue_last() {
        let app = test_app(ArtProtocol::Off);
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Lists    5"), "{text}");
        assert!(text.contains("Queue    6"), "{text}");
    }

    #[test]
    fn the_rail_shows_its_jump_keys_and_a_rule() {
        let app = test_app(ArtProtocol::Off);
        let text = rendered_text(&app, 96, 24);
        assert!(text.contains("▸ Albums"), "{text}");
        // The key used to be clipped off the end of the rail; spacing matters.
        assert!(
            text.contains("Albums   1"),
            "the jump key is visible: {text}"
        );
        assert!(text.contains("Starred  3"), "{text}");
        assert!(
            text.contains('│'),
            "a rule separates the rail from the view"
        );
    }

    #[test]
    fn the_bar_is_there_with_nothing_playing_and_carries_hints() {
        // No track: the bar still exists, says so, and advertises keys.
        let app = test_app(ArtProtocol::Off);
        let text = rendered_text(&app, 96, 24);
        assert!(text.contains("nothing playing"), "{text}");
        assert!(
            text.contains("S sort"),
            "the sort key should be discoverable from the home screen: {text}"
        );
    }

    #[test]
    fn the_layout_key_is_advertised_on_every_view_that_has_a_wall() {
        // A view whose `w` has an effect must say so; the ones without a wall
        // must not offer it.
        for view in [
            View::Albums,
            View::Artists,
            View::Favorites,
            View::ListenLater,
        ] {
            assert!(
                key_hints(view).contains("w layout"),
                "{view:?} has a wall but hides the key: {}",
                key_hints(view)
            );
        }
        for view in [
            View::AlbumDetail,
            View::ArtistDetail,
            View::Playlists,
            View::Search,
            View::PlaylistDetail,
            View::Queue,
        ] {
            assert!(
                !key_hints(view).contains("w "),
                "{view:?} has no wall, so `w` does nothing there: {}",
                key_hints(view)
            );
        }
    }

    #[test]
    fn hints_follow_the_view() {
        let mut app = test_app(ArtProtocol::Off);
        app.view = View::Queue;
        let text = rendered_text(&app, 96, 24);
        assert!(text.contains("> skip"), "{text}");
        assert!(
            !text.contains("w layout"),
            "album hints do not follow you: {text}"
        );
    }
    #[test]
    fn the_rail_marks_the_current_page() {
        let mut app = test_app(ArtProtocol::Off);
        app.view = View::Albums;
        let text = rendered_text(&app, 80, 24);
        assert!(text.contains("Albums"), "{text}");
        assert!(
            text.contains("▸ Albums"),
            "the current page is marked: {text}"
        );
        assert!(
            !text.contains("▸ Artists"),
            "only one entry is marked: {text}"
        );

        app.view = View::Artists;
        let text = rendered_text(&app, 80, 24);
        assert!(text.contains("▸ Artists"), "{text}");
    }

    #[test]
    fn an_album_detail_still_shows_albums_as_the_current_page() {
        // Being inside an album should not make the rail look like nowhere.
        let mut app = test_app(ArtProtocol::Off);
        app.detail = Some(album_with_cover("x"));
        app.view = View::AlbumDetail;
        let text = rendered_text(&app, 80, 24);
        assert!(text.contains("▸ Albums"), "{text}");
    }
    #[test]
    fn the_artists_view_lists_names_and_album_counts() {
        let mut app = test_app(ArtProtocol::Off);
        app.artists = vec![
            undertone_artist("ar-1", "Aphex Twin", 3),
            undertone_artist("ar-2", "Boards of Canada", 1),
        ];
        app.view = View::Artists;
        let text = rendered_text(&app, 80, 24);
        assert!(text.contains("Aphex Twin"), "{text}");
        assert!(text.contains("3 albums"), "{text}");
        assert!(text.contains("1 album"), "singular, not 1 album s: {text}");
    }

    #[test]
    fn a_starred_album_is_marked_in_every_list() {
        let mut app = test_app(ArtProtocol::Off);
        let mut album = album_with_cover("a");
        album.starred = Some("2020-01-01".into());
        app.albums = vec![album];
        app.view = View::Albums;
        assert!(rendered_text(&app, 80, 24).contains('★'));
    }

    #[test]
    fn the_artist_page_lists_that_artist_albums() {
        let mut app = test_app(ArtProtocol::Off);
        app.artist_detail = Some(crate::subsonic::ArtistDetail {
            id: "ar-1".into(),
            name: "Aphex Twin".into(),
            album: vec![album_with_cover("1"), album_with_cover("2")],
            ..Default::default()
        });
        app.view = View::ArtistDetail;
        // Wide enough for the three-pane layout.
        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Aphex Twin"), "{text}");
        assert!(text.contains("album 1"), "{text}");
        assert!(text.contains("albums"), "the pane is titled: {text}");
        assert!(text.contains("similar"), "{text}");
        assert!(text.contains("biography"), "{text}");
        // The keys live in the bar now, not above the list.
        assert!(
            !text.contains("Enter to open"),
            "the page must not repeat the keybindings: {text}"
        );
    }

    #[test]
    fn the_left_pane_shows_its_two_tabs_and_the_song_list() {
        let mut app = test_app(ArtProtocol::Off);
        app.artist_detail = Some(crate::subsonic::ArtistDetail {
            id: "ar-1".into(),
            name: "Aphex Twin".into(),
            album: vec![album_with_cover("1")],
            ..Default::default()
        });
        app.artist_songs = (0..3)
            .map(|i| crate::subsonic::Song {
                id: format!("s{i}"),
                title: format!("top {i}"),
                album: Some("Selected Ambient Works".into()),
                duration: Some(320),
                ..Default::default()
            })
            .collect();
        app.artist_left = crate::app::ArtistLeft::Songs;
        app.view = View::ArtistDetail;

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("albums"), "both tabs are named: {text}");
        assert!(text.contains("top songs"), "{text}");
        assert!(text.contains("top 0"), "the songs are listed: {text}");
        assert!(text.contains("5:20"), "with their length: {text}");
    }

    #[test]
    fn an_artist_without_top_songs_says_so_in_the_pane() {
        let mut app = test_app(ArtProtocol::Off);
        app.artist_detail = Some(crate::subsonic::ArtistDetail {
            id: "ar-1".into(),
            name: "Aphex Twin".into(),
            album: vec![album_with_cover("1")],
            ..Default::default()
        });
        app.artist_left = crate::app::ArtistLeft::Songs;
        app.view = View::ArtistDetail;

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("no top songs"), "{text}");
    }

    #[test]
    fn the_artist_page_shows_similar_artists_and_the_biography() {
        let mut app = test_app(ArtProtocol::Off);
        app.artist_detail = Some(crate::subsonic::ArtistDetail {
            id: "ar-1".into(),
            name: "Aphex Twin".into(),
            album: vec![album_with_cover("1")],
            ..Default::default()
        });
        app.artist_info = Some(crate::subsonic::ArtistInfo {
            biography: Some("Richard David James records as Aphex Twin.".into()),
            similar_artist: vec![
                crate::subsonic::Artist {
                    id: "ar-2".into(),
                    name: "Boards of Canada".into(),
                    ..Default::default()
                },
                crate::subsonic::Artist {
                    id: "ar-3".into(),
                    name: "Autechre".into(),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        app.view = View::ArtistDetail;

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("Boards of Canada"), "{text}");
        assert!(text.contains("Autechre"), "{text}");
        assert!(text.contains("Richard David James"), "{text}");
    }

    #[test]
    fn a_missing_artist_info_leaves_the_page_usable() {
        let mut app = test_app(ArtProtocol::Off);
        app.artist_detail = Some(crate::subsonic::ArtistDetail {
            id: "ar-1".into(),
            name: "Aphex Twin".into(),
            album: vec![album_with_cover("1")],
            ..Default::default()
        });
        app.view = View::ArtistDetail;

        let text = rendered_text(&app, 100, 24);
        assert!(text.contains("album 1"), "the albums still list: {text}");
        assert!(text.contains("no artist info"), "{text}");
    }

    #[test]
    fn every_view_renders_without_panicking_at_a_small_size() {
        let mut app = test_app(ArtProtocol::Off);
        app.albums = vec![album_with_cover("a")];
        app.favorites = vec![album_with_cover("b")];
        app.artists = vec![undertone_artist("ar-1", "A", 1)];
        for view in [
            View::Albums,
            View::Artists,
            View::ArtistDetail,
            View::Favorites,
            View::Queue,
            View::ListenLater,
            View::Help,
        ] {
            app.view = view;
            let _ = rendered_text(&app, 40, 12);
        }
    }

    fn undertone_artist(id: &str, name: &str, albums: u32) -> crate::subsonic::Artist {
        crate::subsonic::Artist {
            id: id.to_string(),
            name: name.to_string(),
            album_count: Some(albums),
            ..Default::default()
        }
    }
}
