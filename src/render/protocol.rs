//! Which image protocol the current terminal speaks, and how big a cell is.

/// Image protocol selected for this terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtProtocol {
    /// foot: sixel only.
    Sixel,
    /// kitty, ghostty, WezTerm: kitty graphics protocol.
    Kitty,
    /// Anything else, including multiplexers: Unicode cells.
    Symbols,
    /// Art disabled entirely.
    Off,
}

impl ArtProtocol {
    pub fn label(self) -> &'static str {
        match self {
            Self::Sixel => "sixel",
            Self::Kitty => "kitty",
            Self::Symbols => "symbols",
            Self::Off => "off",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "sixel" => Some(Self::Sixel),
            "kitty" => Some(Self::Kitty),
            "symbols" => Some(Self::Symbols),
            "off" | "none" => Some(Self::Off),
            _ => None,
        }
    }

    /// True when the protocol places real pixels (so the painter is used).
    pub fn is_pixel(self) -> bool {
        matches!(self, Self::Sixel | Self::Kitty)
    }
}

/// Cell dimensions in pixels. Needed to size an image to a cell rectangle;
/// it cannot be derived from columns and rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellSize {
    pub width: u32,
    pub height: u32,
}

impl CellSize {
    /// Used when the terminal does not report pixel dimensions.
    pub const FALLBACK: Self = Self {
        width: 8,
        height: 17,
    };

    /// Build from reported pixels, falling back per-axis on zero.
    pub fn new(width: u32, height: u32) -> Self {
        let fallback = Self::FALLBACK;
        Self {
            width: if width == 0 { fallback.width } else { width },
            height: if height == 0 { fallback.height } else { height },
        }
    }

    /// Ask the terminal. Requires a TTY; falls back silently when unavailable
    /// (pipes, CI, `TERM=dumb`).
    pub fn detect() -> Self {
        match crossterm::terminal::window_size() {
            Ok(size) => Self::new(u32::from(size.width), u32::from(size.height)),
            Err(_) => Self::FALLBACK,
        }
    }

    /// Pixel block for a rectangle of cells.
    pub fn pixels_for(self, cols: u16, rows: u16) -> (u32, u32) {
        (self.width * u32::from(cols), self.height * u32::from(rows))
    }

    /// How many cells a pixel block covers (rounding up).
    pub fn cells_for(self, width_px: u32, height_px: u32) -> (u16, u16) {
        let cols = width_px.div_ceil(self.width).max(1) as u16;
        let rows = height_px.div_ceil(self.height).max(1) as u16;
        (cols, rows)
    }
}

/// Everything the render layer needs to know about the terminal.
#[derive(Debug, Clone)]
pub struct Caps {
    pub protocol: ArtProtocol,
    pub cell: CellSize,
    pub truecolor: bool,
}

impl Caps {
    pub fn detect(override_protocol: Option<ArtProtocol>) -> Self {
        let protocol =
            override_protocol.unwrap_or_else(|| detect_protocol(|key| std::env::var(key).ok()));
        let truecolor = std::env::var("COLORTERM")
            .map(|value| value.contains("truecolor") || value.contains("24bit"))
            .unwrap_or(false);
        Self {
            protocol,
            cell: CellSize::detect(),
            truecolor,
        }
    }
}

/// Choose a protocol from the environment.
///
/// Takes a lookup function so it is testable without mutating the process
/// environment.
pub fn detect_protocol(get: impl Fn(&str) -> Option<String>) -> ArtProtocol {
    let get = |key: &str| get(key);

    // Multiplexers: kitty graphics does not survive tmux or zellij, and sixel
    // passthrough is fragile. Symbols is the honest answer.
    if get("TMUX").is_some() || get("ZELLIJ").is_some() {
        return ArtProtocol::Symbols;
    }

    // kitty sets this and nothing else does.
    if get("KITTY_WINDOW_ID").is_some() {
        return ArtProtocol::Kitty;
    }

    if let Some(program) = get("TERM_PROGRAM") {
        match program.to_ascii_lowercase().as_str() {
            "ghostty" | "kitty" | "wezterm" => return ArtProtocol::Kitty,
            "foot" => return ArtProtocol::Sixel,
            _ => {}
        }
    }

    if let Some(term) = get("TERM") {
        let term = term.to_ascii_lowercase();
        if term.contains("kitty") {
            return ArtProtocol::Kitty;
        }
        if term.contains("foot") {
            return ArtProtocol::Sixel;
        }
    }

    ArtProtocol::Symbols
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect();
        move |key: &str| map.get(key).cloned()
    }

    #[test]
    fn detects_foot_from_term() {
        assert_eq!(
            detect_protocol(env(&[("TERM", "foot")])),
            ArtProtocol::Sixel
        );
        assert_eq!(
            detect_protocol(env(&[("TERM", "foot-extra")])),
            ArtProtocol::Sixel
        );
    }

    #[test]
    fn detects_kitty_from_window_id() {
        assert_eq!(
            detect_protocol(env(&[("KITTY_WINDOW_ID", "1"), ("TERM", "xterm-256color")])),
            ArtProtocol::Kitty
        );
    }

    #[test]
    fn detects_kitty_and_ghostty_from_term_program() {
        for program in ["ghostty", "Ghostty", "kitty", "WezTerm", "wezterm"] {
            assert_eq!(
                detect_protocol(env(&[
                    ("TERM_PROGRAM", program),
                    ("TERM", "xterm-256color")
                ])),
                ArtProtocol::Kitty,
                "TERM_PROGRAM={program}"
            );
        }
    }

    #[test]
    fn detects_foot_from_term_program() {
        assert_eq!(
            detect_protocol(env(&[("TERM_PROGRAM", "foot"), ("TERM", "xterm-256color")])),
            ArtProtocol::Sixel
        );
    }

    #[test]
    fn multiplexers_fall_back_to_symbols() {
        assert_eq!(
            detect_protocol(env(&[
                ("TMUX", "/tmp/tmux-1000/default,1,0"),
                ("TERM", "foot")
            ])),
            ArtProtocol::Symbols
        );
        assert_eq!(
            detect_protocol(env(&[("ZELLIJ", "0"), ("TERM", "foot")])),
            ArtProtocol::Symbols
        );
    }

    #[test]
    fn unknown_terminals_get_symbols() {
        assert_eq!(
            detect_protocol(env(&[("TERM", "xterm-256color")])),
            ArtProtocol::Symbols
        );
        assert_eq!(detect_protocol(env(&[])), ArtProtocol::Symbols);
    }

    #[test]
    fn protocol_parsing_accepts_expected_names() {
        assert_eq!(ArtProtocol::parse("sixel"), Some(ArtProtocol::Sixel));
        assert_eq!(ArtProtocol::parse(" KITTY "), Some(ArtProtocol::Kitty));
        assert_eq!(ArtProtocol::parse("off"), Some(ArtProtocol::Off));
        assert_eq!(ArtProtocol::parse("nonsense"), None);
    }

    #[test]
    fn only_pixel_protocols_use_the_painter() {
        assert!(ArtProtocol::Sixel.is_pixel());
        assert!(ArtProtocol::Kitty.is_pixel());
        assert!(!ArtProtocol::Symbols.is_pixel());
        assert!(!ArtProtocol::Off.is_pixel());
    }

    #[test]
    fn cell_size_falls_back_per_axis() {
        assert_eq!(CellSize::new(0, 0), CellSize::FALLBACK);
        assert_eq!(
            CellSize::new(9, 0),
            CellSize {
                width: 9,
                height: 17
            }
        );
        assert_eq!(
            CellSize::new(0, 20),
            CellSize {
                width: 8,
                height: 20
            }
        );
        assert_eq!(CellSize::new(8, 17), CellSize::FALLBACK);
    }

    #[test]
    fn pixel_and_cell_conversions_round_trip_reasonably() {
        let cell = CellSize {
            width: 8,
            height: 17,
        };
        assert_eq!(cell.pixels_for(20, 10), (160, 170));
        // 160x160 in 8x17 cells covers 20 columns and 10 rows (rounded up).
        assert_eq!(cell.cells_for(160, 160), (20, 10));
        // Never zero cells.
        assert_eq!(cell.cells_for(1, 1), (1, 1));
    }
}
