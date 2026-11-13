//! Symbol art: covers drawn with characters, for terminals with no graphics.
//!
//! chafa can render an image as coloured text. Sixel and kitty images are
//! painted over the terminal by the painter, but symbol art is just cells, so
//! it goes through ratatui with everything else. The only work is turning the
//! SGR colour codes chafa emits into ratatui styles.

use ratatui::style::{Color, Style};
use ratatui::text::{Line, Span};

/// Parse a chafa symbol blob into exactly `rows` lines of `cols` cells.
///
/// Anything that is not text or a colour is dropped: cursor moves, the
/// cursor-show sequence the worker appends, and the OSC strings chafa writes
/// when it is allowed to probe. Short lines are padded so a cover always
/// occupies its whole slot and never leaves the previous row showing through.
pub fn parse(blob: &[u8], cols: u16, rows: u16, backdrop: [u8; 3]) -> Vec<Line<'static>> {
    let text = String::from_utf8_lossy(blob);
    let width = usize::from(cols);
    let height = usize::from(rows);

    let mut lines: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut run = String::new();
    let mut style = Style::default();
    let mut used = 0usize;

    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\n' => {
                flush(&mut spans, &mut run, style);
                lines.push(Line::from(std::mem::take(&mut spans)));
                used = 0;
                if lines.len() == height {
                    break;
                }
            }
            '\r' => {}
            '\x1b' => {
                // OSC, APC and friends are string sequences: they end with BEL
                // or ST, and their contents are not text.
                if matches!(chars.peek(), Some(']') | Some('P') | Some('_') | Some('^')) {
                    chars.next();
                    while let Some(inner) = chars.next() {
                        if inner == '\x07' {
                            break;
                        }
                        if inner == '\x1b' {
                            if chars.peek() == Some(&'\\') {
                                chars.next();
                            }
                            break;
                        }
                    }
                    continue;
                }
                // ESC [ … m sets colour; every other escape is noise here.
                if chars.peek() == Some(&'[') {
                    chars.next();
                    let mut params = String::new();
                    let mut final_byte = None;
                    for byte in chars.by_ref() {
                        if byte.is_ascii_alphabetic() {
                            final_byte = Some(byte);
                            break;
                        }
                        params.push(byte);
                    }
                    if final_byte == Some('m') {
                        flush(&mut spans, &mut run, style);
                        apply_sgr(&mut style, &params);
                    }
                } else {
                    // Consume the rest of a two-byte escape.
                    chars.next();
                }
            }
            ch if used < width => {
                run.push(ch);
                used += 1;
            }
            _ => {}
        }
    }

    if lines.len() < height {
        flush(&mut spans, &mut run, style);
        if !spans.is_empty() {
            lines.push(Line::from(spans.clone()));
        }
        lines.truncate(height);
    }

    // Pad to a full rectangle: a short line would let the row underneath show.
    let backdrop_style = Style::default().fg(Color::Rgb(backdrop[0], backdrop[1], backdrop[2]));
    for line in &mut lines {
        let len: usize = line
            .spans
            .iter()
            .map(|span| span.content.chars().count())
            .sum();
        if len < width {
            line.spans
                .push(Span::styled(" ".repeat(width - len), backdrop_style));
        }
    }
    while lines.len() < height {
        lines.push(Line::from(Span::styled(" ".repeat(width), backdrop_style)));
    }

    lines
}

fn flush(spans: &mut Vec<Span<'static>>, run: &mut String, style: Style) {
    if run.is_empty() {
        return;
    }
    spans.push(Span::styled(std::mem::take(run), style));
}

/// Apply one SGR parameter string to a style.
///
/// Only colours matter: chafa sets weight and underline never, and losing
/// those would be invisible anyway.
fn apply_sgr(style: &mut Style, params: &str) {
    let nums: Vec<u32> = params
        .split(';')
        .map(|part| part.parse::<u32>().unwrap_or(0))
        .collect();
    let mut index = 0;
    while index < nums.len() {
        match nums[index] {
            0 => *style = Style::default(),
            1 | 2 | 3 | 4 | 5 | 7 | 21 | 22 | 24 | 27 => {}
            n @ 30..=37 => *style = style.fg(Color::Indexed((n - 30) as u8)),
            39 => *style = style.fg(Color::Reset),
            n @ 40..=47 => *style = style.bg(Color::Indexed((n - 40) as u8)),
            49 => *style = style.bg(Color::Reset),
            n @ 90..=97 => *style = style.fg(Color::Indexed((n - 90 + 8) as u8)),
            n @ 100..=107 => *style = style.bg(Color::Indexed((n - 100 + 8) as u8)),
            38 | 48 => {
                let is_fg = nums[index] == 38;
                match nums.get(index + 1) {
                    Some(5) => {
                        if let Some(colour) = nums.get(index + 2) {
                            let colour = Color::Indexed(*colour as u8);
                            *style = if is_fg {
                                style.fg(colour)
                            } else {
                                style.bg(colour)
                            };
                        }
                        index += 2;
                    }
                    Some(2) => {
                        if let (Some(r), Some(g), Some(b)) = (
                            nums.get(index + 2),
                            nums.get(index + 3),
                            nums.get(index + 4),
                        ) {
                            let colour = Color::Rgb(*r as u8, *g as u8, *b as u8);
                            *style = if is_fg {
                                style.fg(colour)
                            } else {
                                style.bg(colour)
                            };
                        }
                        index += 4;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        index += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines
            .iter()
            .map(|line| {
                line.spans
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>()
            })
            .collect()
    }

    #[test]
    fn text_is_split_into_rows_and_padded() {
        let blob = b"abcd\nef\n";
        let lines = parse(blob, 4, 3, [0, 0, 0]);
        assert_eq!(text_of(&lines), vec!["abcd", "ef  ", "    "]);
    }

    #[test]
    fn colour_runs_become_separate_spans() {
        let blob = b"\x1b[31mab\x1b[0mcd";
        let lines = parse(blob, 6, 1, [0, 0, 0]);
        let spans = &lines[0].spans;
        assert_eq!(spans[0].content.as_ref(), "ab");
        assert_eq!(spans[0].style.fg, Some(Color::Indexed(1)));
        assert_eq!(spans[1].content.as_ref(), "cd");
        assert_eq!(spans[1].style.fg, None);
    }

    #[test]
    fn two_fifty_six_colours_and_truecolour_are_understood() {
        let lines = parse(b"\x1b[38;5;208mx\x1b[0m", 2, 1, [0, 0, 0]);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Indexed(208)));

        let lines = parse(b"\x1b[38;2;10;20;30my\x1b[0m", 2, 1, [0, 0, 0]);
        assert_eq!(lines[0].spans[0].style.fg, Some(Color::Rgb(10, 20, 30)));
    }

    #[test]
    fn cursor_moves_and_osc_strings_are_dropped() {
        // The worker appends a cursor-show, and OSC can turn up if a probe
        // slipped through; neither may reach a cell.
        let blob = b"\x1b[?25lab\x1b[2;3Hc\x1b]0;title\x07d";
        let lines = parse(blob, 6, 1, [0, 0, 0]);
        assert_eq!(text_of(&lines), vec!["abcd  "]);
    }

    #[test]
    fn rows_beyond_the_slot_are_cut() {
        let blob = b"one\ntwo\nthree\n";
        let lines = parse(blob, 5, 2, [0, 0, 0]);
        assert_eq!(text_of(&lines), vec!["one  ", "two  "]);
    }

    #[test]
    fn wide_characters_do_not_overflow_the_slot() {
        // One character per cell, whatever its display width: the cover is a
        // fixed slot and overrunning it would paint over the title.
        let blob = "日本語\n".as_bytes();
        let lines = parse(blob, 2, 1, [0, 0, 0]);
        assert_eq!(text_of(&lines).len(), 1);
        assert!(
            lines[0]
                .spans
                .iter()
                .map(|s| s.content.chars().count())
                .sum::<usize>()
                >= 2
        );
    }
}
