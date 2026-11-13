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

use crate::app::{
    AlbumLayout, App, View, GRID_THUMB_H, GRID_THUMB_W, GRID_TILE_H, GRID_TILE_W, ROW_HEIGHT,
    THUMB_CELLS, THUMB_ROWS,
};
use crate::art::ArtService;
use crate::render::{
    blank_sixel, kitty_delete, kitty_id, ArtProtocol, FramePlan, PaintOp, Placement,
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

/// Rows the now-playing bar occupies, top border included: two lines beside
/// the cover — track and clock, then progress plus album/format/next.
pub const NOW_PLAYING_HEIGHT: u16 = 3;

/// Width of the navigation rail, and the narrowest window that keeps it.
pub const SIDEBAR_W: u16 = 14;
pub const SIDEBAR_MIN_WIDTH: u16 = 64;
/// One column of rule between the rail and the view, then one of air.
const SIDEBAR_GAP: u16 = 2;

/// Where each part of the screen lives.
struct Shell {
    /// The navigation rail, absent on narrow terminals.
    sidebar: Option<Rect>,
    /// The rule that separates the rail from the view.
    divider: Option<Rect>,
    /// The view itself.
    content: Rect,
    status: Rect,
    /// Always present: an empty bar still shows the key hints.
    now_playing: Rect,
}

/// Split the screen into [sidebar | content], status, and now-playing.
fn shell_layout(area: Rect) -> Shell {
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(1 + NOW_PLAYING_HEIGHT),
        ])
        .split(area);
    let bottom = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1),
            Constraint::Length(NOW_PLAYING_HEIGHT),
        ])
        .split(rows[1]);

    // On a small terminal a rail would eat a third of the width; the status
    // line still says which sort is in use, so the view stays usable.
    if area.width < SIDEBAR_MIN_WIDTH {
        return Shell {
            sidebar: None,
            divider: None,
            content: rows[0],
            status: bottom[0],
            now_playing: bottom[1],
        };
    }
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Length(SIDEBAR_W + SIDEBAR_GAP),
            Constraint::Min(20),
        ])
        .split(rows[0]);
    // The rail keeps its own rect; the rule and the gap are drawn into the
    // first two columns of what follows, so nothing ever touches the view.
    Shell {
        sidebar: Some(Rect {
            width: SIDEBAR_W,
            ..columns[0]
        }),
        divider: Some(Rect {
            x: columns[0].x + SIDEBAR_W,
            width: 1,
            ..columns[0]
        }),
        content: columns[1],
        status: bottom[0],
        now_playing: bottom[1],
    }
}

/// The views reachable from the rail, with their jump keys.
const NAV_ITEMS: [(View, &str, char); 5] = [
    (View::Albums, "Albums", '1'),
    (View::Artists, "Artists", '2'),
    (View::Favorites, "Starred", '3'),
    (View::ListenLater, "Later", '4'),
    (View::Queue, "Queue", '5'),
];

/// Which rail entry a view belongs to: an album detail is still "Albums".
fn nav_owner(view: View) -> View {
    match view {
        View::AlbumDetail => View::Albums,
        View::ArtistDetail => View::Artists,
        other => other,
    }
}

fn draw_sidebar(frame: &mut Frame, app: &App, area: Rect) {
    let current = nav_owner(app.view);
    if area.height > 2 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "undertone",
                Style::default().fg(ACCENT).add_modifier(Modifier::BOLD),
            ))),
            Rect { height: 1, ..area },
        );
    }
    for (index, (view, label, key)) in NAV_ITEMS.iter().enumerate() {
        // One blank row under the title, so the rail does not start flush
        // against it.
        let y = area.y + 2 + index as u16;
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
            // Two columns for the marker, ten for the label, one for the key:
            // the rail is thirteen wide, and the key used to be clipped off
            // the end of it.
            Span::styled(
                format!("{label:<10}"),
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

/// The bar's content area, below its top border.
fn now_playing_inner(area: Rect) -> Rect {
    Block::default().borders(Borders::TOP).inner(area)
}

/// Cells the bar's cover occupies. Deliberately smaller than a list
/// thumbnail: the bar is two lines tall, and a three-row cover would push the
/// progress line off it.
const BAR_COVER_CELLS: u16 = 4;
const BAR_COVER_ROWS: u16 = 2;

/// Where the cover sits inside the bar.
fn now_playing_cover_rect(inner: Rect) -> Rect {
    Rect {
        x: inner.x,
        y: inner.y,
        width: BAR_COVER_CELLS.min(inner.width / 2),
        height: BAR_COVER_ROWS.min(inner.height),
    }
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
    if !app.protocol.is_pixel() {
        return;
    }

    let shell = shell_layout(area);
    let main = shell.content;
    let now = shell.now_playing;
    app.list_area = (main.width, main.height);

    match app.view {
        View::Albums => {
            app.ensure_visible(main.width, main.height);
            let colors = app.settings.thumb_colors;
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
                    let cols = app.grid_columns(main.width).max(1) as u16;
                    let tile_rows = (main.height / GRID_TILE_H).max(1);
                    let per_page = usize::from(cols * tile_rows);
                    for slot in 0..per_page {
                        let index = app.scroll + slot;
                        let Some(album) = app.albums.get(index) else {
                            break;
                        };
                        let Some(cover) = album.cover_art.as_deref() else {
                            continue;
                        };
                        let x = main.x + (slot as u16 % cols) * GRID_TILE_W;
                        let y = main.y + (slot as u16 / cols) * GRID_TILE_H;
                        let key = app.art.request(
                            cover,
                            u32::from(GRID_THUMB_W),
                            u32::from(GRID_THUMB_H),
                            colors,
                            index >= app.selected + per_page,
                        );
                        if !app.art.is_ready(&key) {
                            continue;
                        }
                        app.placements
                            .push(Placement::new(key, y, x, GRID_THUMB_H, GRID_THUMB_W));
                    }
                }
            }
        }
        View::AlbumDetail => {
            if let Some(album) = app.detail.as_ref() {
                if let Some(cover) = album.cover_art.as_deref() {
                    let colors = app.settings.cover_colors;
                    let key = app.art.request(cover, 20, 8, colors, false);
                    if app.art.is_ready(&key) {
                        app.placements
                            .push(Placement::new(key, main.y, main.x, 8, 20));
                    }
                }
            }
        }
        View::Artists => {
            app.ensure_artist_visible(main.height);
            let covers: Vec<Option<String>> = app
                .artists
                .iter()
                .map(|artist| artist.cover_art.clone())
                .collect();
            collect_list_placements(
                app,
                main,
                &covers,
                app.artist_scroll,
                app.artist_selected + visible_albums(main),
            );
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
            app.ensure_favorite_visible(main.height);
            let covers: Vec<Option<String>> = app
                .favorites
                .iter()
                .map(|album| album.cover_art.clone())
                .collect();
            collect_list_placements(
                app,
                main,
                &covers,
                app.favorite_scroll,
                app.favorite_selected + visible_albums(main),
            );
        }
        View::ListenLater => {
            app.ensure_listen_visible(main.height);
            let visible = visible_albums(main);
            let covers: Vec<Option<String>> = app
                .listen_albums
                .iter()
                .map(|album| album.cover_art.clone())
                .collect();
            collect_list_placements(
                app,
                main,
                &covers,
                app.listen_scroll,
                app.listen_selected + visible,
            );
        }
        View::Queue | View::Help => {}
    }

    // The now-playing cover lives in the bottom bar and is drawn under every
    // view, so it is the last placement added rather than a per-view concern.
    {
        let now_area = now;
        if let Some(track) = app.now.track.as_ref() {
            if let Some(cover) = track.cover_art.as_deref() {
                let rect = now_playing_cover_rect(now_playing_inner(now_area));
                if rect.width > 0 && rect.height > 0 {
                    let key = app.art.request(
                        cover,
                        u32::from(rect.width),
                        u32::from(rect.height),
                        app.settings.thumb_colors,
                        false,
                    );
                    if app.art.is_ready(&key) {
                        app.placements.push(Placement::new(
                            key,
                            rect.y,
                            rect.x,
                            rect.height,
                            rect.width,
                        ));
                    }
                }
            }
        }
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
            u32::from(THUMB_CELLS),
            u32::from(THUMB_ROWS),
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
            THUMB_ROWS,
            THUMB_CELLS,
        ));
    }
}

pub fn emit_erases(
    terminal: &mut Term,
    plan: &FramePlan,
    protocol: ArtProtocol,
    backdrop: [u8; 3],
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
                    queue!(terminal.backend_mut(), MoveTo(*col, *row))?;
                    terminal
                        .backend_mut()
                        .write_all(&blank_sixel(*rows, *cols, backdrop))?;
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
    let main = shell.content;
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

    match app.view {
        View::Albums => draw_albums(frame, app, main),
        View::AlbumDetail => draw_album_detail(frame, app, main),
        View::Queue => draw_queue(frame, app, main),
        View::Artists => draw_artists(frame, app, main),
        View::ArtistDetail => draw_artist_detail(frame, app, main),
        View::Favorites => draw_favorites(frame, app, main),
        View::ListenLater => draw_listen_later(frame, app, main),
        View::Help => draw_help(frame, main),
    }

    draw_status_bar(frame, app, shell.status);
    draw_now_playing(frame, app, shell.now_playing);
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
    draw_album_rows(frame, app, area, &app.albums, app.selected, app.scroll);
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
    draw_album_rows(
        frame,
        app,
        area,
        &app.listen_albums,
        app.listen_selected,
        app.listen_scroll,
    );
}

/// Album rows, shared by the library list, listen later and favourites.
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

    let text_area = Rect {
        x: row.x + THUMB_CELLS + 1,
        y: row.y,
        width: row.width.saturating_sub(THUMB_CELLS + 1),
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
    let thumb_key = cover.map(|cover| {
        ArtService::key(
            cover,
            u32::from(THUMB_CELLS),
            u32::from(THUMB_ROWS),
            app.settings.thumb_colors,
            app.protocol,
        )
    });
    let has_art = thumb_key
        .as_deref()
        .map(|key| app.art.is_ready(key))
        .unwrap_or(false);
    if !has_art {
        let placeholder = Rect {
            x: row.x,
            y: row.y,
            width: THUMB_CELLS,
            height: THUMB_ROWS,
        };
        draw_thumb_placeholder(frame, placeholder, cover.is_some());
    }
    has_art
}

fn draw_artists(frame: &mut Frame, app: &App, area: Rect) {
    if app.artists.is_empty() {
        frame.render_widget(
            Paragraph::new("No artists loaded. Press A to fetch them.")
                .style(Style::default().fg(MUTED)),
            area,
        );
        return;
    }
    for slot in 0..visible_albums(area) {
        let index = app.artist_scroll + slot;
        let Some(artist) = app.artists.get(index) else {
            break;
        };
        let subtitle = match artist.album_count {
            Some(1) => "1 album".to_string(),
            Some(count) => format!("{count} albums"),
            None => String::new(),
        };
        draw_row(
            frame,
            app,
            area,
            slot,
            artist.cover_art.as_deref(),
            &artist.name,
            &subtitle,
            None,
            index == app.artist_selected,
            artist.starred.is_some(),
        );
    }
}

fn draw_artist_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(artist) = app.artist_detail.as_ref() else {
        return;
    };
    // Two rows for the header keeps the album rows on the same two-line grid
    // as everywhere else.
    let albums_area = artist_albums_area(area);
    let header = Rect {
        height: albums_area.y - area.y,
        ..area
    };
    let count = match artist.album.len() {
        1 => "1 album".to_string(),
        n => format!("{n} albums"),
    };
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(artist.name.clone(), title_style())),
            Line::from(Span::styled(
                format!("{count}   — Enter to open, h back"),
                Style::default().fg(MUTED),
            )),
        ]),
        header,
    );

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
    draw_album_rows(
        frame,
        app,
        area,
        &app.favorites,
        app.favorite_selected,
        app.favorite_scroll,
    );
}

fn draw_albums_grid(frame: &mut Frame, app: &App, area: Rect) {
    let cols = app.grid_columns(area.width).max(1) as u16;
    let tile_rows = (area.height / GRID_TILE_H).max(1);
    let per_page = usize::from(cols * tile_rows);

    for slot in 0..per_page {
        let index = app.scroll + slot;
        let Some(album) = app.albums.get(index) else {
            break;
        };
        let is_selected = index == app.selected;
        let x = area.x + (slot as u16 % cols) * GRID_TILE_W;
        let y = area.y + (slot as u16 / cols) * GRID_TILE_H;

        let thumb_key = album.cover_art.as_deref().map(|cover| {
            ArtService::key(
                cover,
                u32::from(GRID_THUMB_W),
                u32::from(GRID_THUMB_H),
                app.settings.thumb_colors,
                app.protocol,
            )
        });
        let has_art = thumb_key
            .as_deref()
            .map(|key| app.art.is_ready(key))
            .unwrap_or(false);
        if !has_art {
            let placeholder = Rect {
                x,
                y,
                width: GRID_THUMB_W,
                height: GRID_THUMB_H,
            };
            draw_thumb_placeholder(frame, placeholder, album.cover_art.is_some());
        }

        // One line of text under the cover; the tile is too narrow for more.
        let label_area = Rect {
            x,
            y: y + GRID_THUMB_H,
            width: GRID_THUMB_W,
            height: 1,
        };
        let label = if is_selected {
            format!("▸{}", album.name)
        } else {
            format!(" {}", album.name)
        };
        frame.render_widget(
            Paragraph::new(label).style(if is_selected {
                selected_style()
            } else {
                Style::default().fg(MUTED)
            }),
            label_area,
        );
    }
}

fn draw_album_detail(frame: &mut Frame, app: &App, area: Rect) {
    let Some(album) = app.detail.as_ref() else {
        frame.render_widget(Paragraph::new("No album"), area);
        return;
    };

    let header = Rect {
        height: area.height.min(9),
        ..area
    };
    let body = Rect {
        y: area.y + header.height,
        height: area.height.saturating_sub(header.height),
        ..area
    };

    let info_x = area.x + 21;
    let info_width = area.width.saturating_sub(22);
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
        Line::from(""),
        Line::from(Span::styled(
            "Enter play · Space pause · > next",
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
    let current = app.queue.position();
    let rows = inner.height as usize;
    let offset = current.unwrap_or(0).saturating_sub(rows.saturating_sub(1));

    for slot in 0..rows {
        let index = offset + slot;
        let Some(item) = items.get(index) else {
            break;
        };
        let is_current = Some(index) == current;
        let marker = if is_current { "▶ " } else { "  " };
        let line = Line::from(vec![
            Span::styled(marker, Style::default().fg(ACCENT)),
            Span::styled(
                item.title.clone(),
                if is_current {
                    selected_style()
                } else {
                    Style::default()
                },
            ),
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
        Line::from("h / l      move left / right (grid) · h back"),
        Line::from("w          toggle list / grid"),
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

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let (text, style) = match app.status.as_ref() {
        Some(status) if status.at.elapsed() < crate::app::STATUS_TTL => (
            status.text.clone(),
            if status.error {
                Style::default().fg(Color::Red)
            } else {
                Style::default().fg(ACCENT)
            },
        ),
        // The server address used to lead this line. It is fixed at startup
        // and says nothing while you are listening, so it gave up its space to
        // the play state.
        _ => (
            format!(
                "{} · art {} · shuffle {} · repeat {} · vol {}{}{}",
                app.sort_label(),
                app.protocol.label(),
                if app.queue.is_shuffled() { "on" } else { "off" },
                app.queue.repeat().label(),
                app.volume,
                if app.muted { " muted" } else { "" },
                if app.settings.auto_dj {
                    " · autodj"
                } else {
                    ""
                },
            ),
            Style::default().fg(MUTED),
        ),
    };
    frame.render_widget(Paragraph::new(Line::from(text)).style(style), area);
}

fn draw_now_playing(frame: &mut Frame, app: &App, area: Rect) {
    let block = Block::default().borders(Borders::TOP);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width < 12 || inner.height < 2 {
        return;
    }

    // Hints live on the right of the second line. They are the answer to "what
    // can I press here", so they get reserved space rather than being the first
    // thing truncated.
    let hints = key_hints(app.view);
    let hint_w = if hints.is_empty() || inner.width < 70 {
        0
    } else {
        (hints.chars().count() as u16 + 1).min(inner.width / 2)
    };
    let body_w = inner.width - hint_w;

    let cover_rect = now_playing_cover_rect(Rect {
        width: body_w,
        ..inner
    });

    let Some(track) = app.now.track.as_ref() else {
        // No track: an empty slot, one line saying so, and the hints.
        frame.render_widget(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::DarkGray)),
            Rect {
                width: cover_rect.width.max(2),
                height: cover_rect.height.max(2),
                ..cover_rect
            },
        );
        let text_x = cover_rect.x + cover_rect.width + 1;
        if body_w > cover_rect.width + 2 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "nothing playing — Enter on an album to start",
                    Style::default().fg(MUTED),
                ))),
                Rect {
                    x: text_x,
                    y: inner.y,
                    width: body_w.saturating_sub(cover_rect.width + 1),
                    height: 1,
                },
            );
        }
        draw_hints(frame, inner, body_w, hint_w, hints);
        return;
    };

    let cover_w = cover_rect.width;
    if !app.protocol.is_pixel() || track.cover_art.is_none() {
        draw_thumb_placeholder(frame, cover_rect, track.cover_art.is_some());
    } else {
        for row in cover_rect.y..cover_rect.y + cover_rect.height {
            for col in cover_rect.x..cover_rect.x + cover_rect.width {
                frame.buffer_mut()[(col, row)].set_symbol(" ");
            }
        }
    }

    let text_x = cover_rect.x + cover_w + 1;
    let text_w = inner.x + body_w - text_x;
    if text_w < 8 {
        draw_hints(frame, inner, body_w, hint_w, hints);
        return;
    }

    // Line one: track, artist, and the clock flush right.
    let clock = format!(
        "{} / {}",
        format_duration(app.now.position.round() as u32),
        format_duration(app.now.duration.round() as u32)
    );
    let clock_w = (clock.chars().count() as u16).min(text_w);
    let title_w = text_w.saturating_sub(clock_w + 1);

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
                truncate_chars(&title, usize::from(title_w).saturating_sub(2)),
                title_style(),
            ),
        ])),
        Rect {
            x: text_x,
            y: inner.y,
            width: title_w,
            height: 1,
        },
    );
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(clock, Style::default().fg(MUTED))))
            .alignment(Alignment::Right),
        Rect {
            x: text_x + title_w,
            y: inner.y,
            width: clock_w,
            height: 1,
        },
    );

    // Line two: progress, then album, container and what plays next.
    let bar_w = (text_w / 4).clamp(8, 26).min(text_w.saturating_sub(10));
    draw_thin_progress(
        frame,
        app,
        Rect {
            x: text_x,
            y: inner.y + 1,
            width: bar_w,
            height: 1,
        },
    );

    let mut meta = String::new();
    if let Some(album) = track.album.as_deref().filter(|a| !a.is_empty()) {
        meta.push_str(album);
    }
    if let Some(format) = track.format_label() {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str(&format);
    }
    if app.now.paused {
        if !meta.is_empty() {
            meta.push_str(" · ");
        }
        meta.push_str("paused");
    }
    let upcoming = app.queue.upcoming(1);
    if !upcoming.is_empty() {
        let titles = upcoming
            .iter()
            .map(|item| item.title.clone())
            .collect::<Vec<_>>()
            .join(" · ");
        if !meta.is_empty() {
            meta.push_str("   ");
        }
        meta.push_str("next  ");
        meta.push_str(&titles);
    }

    if !meta.is_empty() && text_w > bar_w + 2 {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_chars(&meta, usize::from(text_w - bar_w - 2)),
                Style::default().fg(MUTED),
            ))),
            Rect {
                x: text_x + bar_w + 2,
                y: inner.y + 1,
                width: text_w - bar_w - 2,
                height: 1,
            },
        );
    }

    draw_hints(frame, inner, body_w, hint_w, hints);
}

/// Keys worth knowing on this screen, shown at the right of the bar.
///
/// The bar is always visible, so this is the only place the app advertises its
/// own bindings; a key you cannot discover may as well not exist.
fn key_hints(view: View) -> &'static str {
    match view {
        View::Albums => "S sort · w grid · Enter play · ? help",
        View::AlbumDetail => "Enter play · * star · h back",
        View::Artists => "S sort · Enter open · 1-5 jump",
        View::ArtistDetail => "Enter open · S sort · h back",
        View::Favorites => "Enter open · * unstar · h back",
        View::ListenLater => "a remove · Enter open · h back",
        View::Queue => "Enter play · > skip · h back",
        View::Help => "",
    }
}

fn draw_hints(frame: &mut Frame, inner: Rect, body_w: u16, hint_w: u16, hints: &str) {
    if hint_w == 0 || hints.is_empty() {
        return;
    }
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            hints,
            Style::default().fg(Color::DarkGray),
        )))
        .alignment(Alignment::Right),
        Rect {
            x: inner.x + body_w,
            y: inner.y + inner.height - 1,
            width: hint_w,
            height: 1,
        },
    );
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

    #[test]
    fn durations_format_as_minutes_and_seconds() {
        assert_eq!(format_duration(0), "0:00");
        assert_eq!(format_duration(9), "0:09");
        assert_eq!(format_duration(65), "1:05");
        assert_eq!(format_duration(600), "10:00");
        assert_eq!(format_duration(3599), "59:59");
    }

    #[test]
    fn layout_reserves_a_status_bar_and_an_always_visible_bar() {
        let area = Rect::new(0, 0, 80, 24);
        let shell = shell_layout(area);

        assert_eq!(shell.status.height, 1);
        // The bar is always there, even with nothing playing: it carries the
        // key hints and an empty slot for the cover.
        assert_eq!(shell.now_playing.height, NOW_PLAYING_HEIGHT);
        assert_eq!(shell.content.height, 24 - 1 - NOW_PLAYING_HEIGHT);

        // The rail sits to the left of the content, with a rule between them.
        let sidebar = shell.sidebar.expect("rail");
        assert_eq!(sidebar.width, SIDEBAR_W);
        assert_eq!(sidebar.x, area.x);
        let divider = shell.divider.expect("divider");
        assert_eq!(divider.x, sidebar.x + SIDEBAR_W);
        assert_eq!(divider.height, shell.content.height);
        assert_eq!(
            shell.content.x,
            divider.x + SIDEBAR_GAP,
            "the view never touches the rail"
        );
    }

    #[test]
    fn the_now_playing_cover_reuses_the_list_thumbnail_shape() {
        let inner = now_playing_inner(Rect::new(0, 19, 100, NOW_PLAYING_HEIGHT));
        let cover = now_playing_cover_rect(inner);
        // The bar's cover is its own size: two lines tall, so the progress
        // rule and the queue preview still fit beside it.
        assert_eq!(cover.width, BAR_COVER_CELLS);
        assert_eq!(cover.height, BAR_COVER_ROWS);
        assert_eq!(cover.x, 0);
        assert_eq!(cover.y, 20);
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
        warm_art(&mut app, u32::from(THUMB_CELLS), u32::from(THUMB_ROWS));
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
        warm_art(&mut app, u32::from(THUMB_CELLS), u32::from(THUMB_ROWS));
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
        warm_art(&mut app, u32::from(GRID_THUMB_W), u32::from(GRID_THUMB_H));
        let area = Rect::new(0, 0, 100, 30);

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
    fn symbols_mode_places_no_pixels_at_all() {
        let mut app = test_app(ArtProtocol::Symbols);
        app.albums = vec![album_with_cover("a")];
        collect_placements(&mut app, Rect::new(0, 0, 100, 30));
        assert!(app.placements.is_empty());
        assert!(app.painter.plan(&app.placements).is_empty());
    }

    #[test]
    fn the_rail_shows_its_jump_keys_and_a_rule() {
        let app = test_app(ArtProtocol::Off);
        let text = rendered_text(&app, 96, 24);
        assert!(text.contains("▸ Albums"), "{text}");
        // The key used to be clipped off the end of the rail; spacing matters.
        assert!(
            text.contains("Albums    1"),
            "the jump key is visible: {text}"
        );
        assert!(text.contains("Starred   3"), "{text}");
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
    fn hints_follow_the_view() {
        let mut app = test_app(ArtProtocol::Off);
        app.view = View::Queue;
        let text = rendered_text(&app, 96, 24);
        assert!(text.contains("> skip"), "{text}");
        assert!(
            !text.contains("w grid"),
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
        let text = rendered_text(&app, 80, 24);
        assert!(text.contains("Aphex Twin"), "{text}");
        assert!(text.contains("2 albums"), "{text}");
        assert!(
            text.contains("Enter to open"),
            "the page should say how to open: {text}"
        );
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
