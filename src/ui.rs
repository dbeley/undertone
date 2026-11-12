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
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Gauge, Paragraph, Wrap};
use ratatui::Frame;
use ratatui::Terminal;

use crate::app::{App, View, ROW_HEIGHT, THUMB_CELLS, THUMB_ROWS};
use crate::art::ArtService;
use crate::render::{FramePlan, PaintOp, Placement};

pub type Term = Terminal<CrosstermBackend<Stdout>>;

const ACCENT: Color = Color::Magenta;
const MUTED: Color = Color::DarkGray;

fn title_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD)
}

fn selected_style() -> Style {
    Style::default().add_modifier(Modifier::BOLD).fg(ACCENT)
}

/// Split the screen into [main, status, now-playing].
fn shell_layout(area: Rect, has_track: bool) -> (Rect, Rect, Option<Rect>) {
    let bars = if has_track { 4 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(3), Constraint::Length(bars)])
        .split(area);
    if has_track {
        let bottom = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(3)])
            .split(chunks[1]);
        (chunks[0], bottom[0], Some(bottom[1]))
    } else {
        (chunks[0], chunks[1], None)
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
    if !app.protocol.is_pixel() {
        return;
    }

    let (main, _, _) = shell_layout(area, app.now.track.is_some());
    app.list_area = (main.width, main.height);

    match app.view {
        View::Albums => {
            app.ensure_visible(main.height as usize);
            let visible = visible_albums(main);
            let colors = app.settings.thumb_colors;
            for slot in 0..visible {
                let index = app.scroll + slot;
                let Some(album) = app.albums.get(index) else {
                    break;
                };
                let Some(cover) = album.cover_art.as_deref() else {
                    continue;
                };
                let key = app.art.request(
                    cover,
                    u32::from(THUMB_CELLS),
                    u32::from(THUMB_ROWS),
                    colors,
                    index >= app.selected + visible,
                );
                app.placements.push(Placement::new(
                    key,
                    main.y + slot as u16 * ROW_HEIGHT,
                    main.x,
                    THUMB_ROWS,
                    THUMB_CELLS,
                ));
            }
        }
        View::AlbumDetail => {
            if let Some(album) = app.detail.as_ref() {
                if let Some(cover) = album.cover_art.as_deref() {
                    let colors = app.settings.cover_colors;
                    let key = app.art.request(cover, 20, 8, colors, false);
                    app.placements
                        .push(Placement::new(key, main.y, main.x, 8, 20));
                }
            }
        }
        View::Queue | View::Help => {}
    }
}

pub fn emit_erases(terminal: &mut Term, plan: &FramePlan) -> io::Result<()> {
    for op in &plan.erases {
        if let PaintOp::Erase {
            row,
            col,
            rows,
            cols,
        } = op
        {
            let blanks = " ".repeat(usize::from(*cols));
            for offset in 0..*rows {
                queue!(terminal.backend_mut(), MoveTo(*col, row + offset))?;
                terminal.backend_mut().write_all(blanks.as_bytes())?;
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
    let (main, status, now) = shell_layout(area, app.now.track.is_some());

    match app.view {
        View::Albums => draw_albums(frame, app, main),
        View::AlbumDetail => draw_album_detail(frame, app, main),
        View::Queue => draw_queue(frame, app, main),
        View::Help => draw_help(frame, main),
    }

    draw_status_bar(frame, app, status);
    if let Some(now_area) = now {
        draw_now_playing(frame, app, now_area);
    }
}

fn draw_albums(frame: &mut Frame, app: &App, area: Rect) {
    let visible = visible_albums(area);
    for slot in 0..visible {
        let index = app.scroll + slot;
        let Some(album) = app.albums.get(index) else {
            break;
        };
        let row = album_row_rect(area, slot);
        let is_selected = index == app.selected;

        // Thumbnail column: draw a placeholder only when no blob is painted
        // there, so we never overwrite a placed image with text.
        let thumb_key = album.cover_art.as_deref().map(|cover| {
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
            let glyph = if album.cover_art.is_some() {
                "▚ "
            } else {
                "  "
            };
            let widget = Paragraph::new(glyph.repeat(usize::from(THUMB_CELLS) / 2))
                .style(Style::default().fg(MUTED));
            frame.render_widget(widget, placeholder);
        }

        // Text column.
        let text_area = Rect {
            x: row.x + THUMB_CELLS + 1,
            y: row.y,
            width: row.width.saturating_sub(THUMB_CELLS + 1),
            height: ROW_HEIGHT,
        };
        let year = album
            .year
            .map(|year| format!(" · {year}"))
            .unwrap_or_default();
        let artist = album.artist.clone().unwrap_or_default();
        let lines = vec![
            Line::from(Span::styled(
                album.name.clone(),
                if is_selected {
                    selected_style()
                } else {
                    title_style()
                },
            )),
            Line::from(Span::styled(
                format!("{artist}{year}"),
                Style::default().fg(MUTED),
            )),
        ];
        frame.render_widget(Paragraph::new(lines), text_area);

        if is_selected {
            // A marker in the leftmost column keeps selection visible even
            // when the row has art covering the thumbnail cell.
            let marker = Rect {
                x: row.x,
                y: row.y,
                width: 1,
                height: 1,
            };
            if !has_art {
                frame.render_widget(Paragraph::new("▸").style(selected_style()), marker);
            }
        }
    }

    if app.albums.is_empty() {
        frame.render_widget(
            Paragraph::new("No albums. Press R to reload, S to change sort.")
                .style(Style::default().fg(MUTED)),
            area,
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
        Line::from("g / G      first / last"),
        Line::from("Enter      open album · play track"),
        Line::from("Space      play / pause"),
        Line::from("> / <      next / previous track"),
        Line::from("f / b      seek forward / back 10s"),
        Line::from("s / r      shuffle / repeat"),
        Line::from("+ / -      volume"),
        Line::from("m          mute"),
        Line::from("Q          queue"),
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
        _ => (
            format!(
                "{} · sort {} · ? help",
                app.settings.server,
                app.sort.as_str()
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

    let Some(track) = app.now.track.as_ref() else {
        return;
    };

    let state = if app.now.paused { "paused" } else { "playing" };
    let artist = track.artist.clone().unwrap_or_default();
    let line = Line::from(vec![
        Span::styled(
            if app.now.paused { "❚❚ " } else { "▶ " },
            Style::default().fg(ACCENT),
        ),
        Span::styled(track.title.clone(), title_style()),
        Span::styled(format!(" — {artist}"), Style::default().fg(MUTED)),
        Span::styled(
            format!(
                "   {} / {}  {state}",
                format_duration(app.now.position.round() as u32),
                format_duration(app.now.duration.round() as u32)
            ),
            Style::default().fg(MUTED),
        ),
    ]);
    let text_row = Rect {
        x: inner.x,
        y: inner.y,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(Paragraph::new(line), text_row);

    let ratio = if app.now.duration > 0.0 {
        (app.now.position / app.now.duration).clamp(0.0, 1.0)
    } else {
        0.0
    };
    let gauge_area = Rect {
        x: inner.x,
        y: inner.y + 1,
        width: inner.width,
        height: 1,
    };
    frame.render_widget(
        Gauge::default()
            .ratio(ratio)
            .gauge_style(Style::default().fg(ACCENT))
            .label(""),
        gauge_area,
    );
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
    fn layout_reserves_a_status_bar_and_optional_now_playing() {
        let area = Rect::new(0, 0, 80, 24);
        let (main, status, now) = shell_layout(area, false);
        assert_eq!(main.height, 23);
        assert_eq!(status.height, 1);
        assert!(now.is_none());

        let (main, status, now) = shell_layout(area, true);
        assert_eq!(main.height, 20);
        assert_eq!(status.height, 1);
        assert_eq!(now.map(|r| r.height), Some(3));
    }
}
