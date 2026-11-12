//! chafa subprocess encoding.
//!
//! One process can render several images, which amortises process startup —
//! measured at ~19 ms on an N100 and roughly 5-8x that on a Pi 2 class board.
//! Batched output is split back into per-image blobs on the protocol
//! terminator.

use std::path::PathBuf;
use std::process::Command;

use super::protocol::ArtProtocol;

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("chafa failed: {0}")]
    Chafa(String),
    #[error("chafa produced {got} images, expected {expected}")]
    SplitMismatch { expected: usize, got: usize },
    #[error("requests in one batch must share size and colour depth")]
    MixedBatch,
    #[error("io: {0}")]
    Io(String),
}

/// One image to encode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderRequest {
    pub source: PathBuf,
    pub cols: u32,
    pub rows: u32,
    pub colors: u32,
}

impl RenderRequest {
    pub fn new(source: impl Into<PathBuf>, cols: u32, rows: u32, colors: u32) -> Self {
        Self {
            source: source.into(),
            cols,
            rows,
            colors,
        }
    }

    /// Requests can share a chafa invocation only if these match.
    pub fn batch_shape(&self) -> (u32, u32, u32) {
        (self.cols, self.rows, self.colors)
    }
}

/// Something that turns images into terminal escape bytes.
///
/// A trait so the worker can be tested without chafa present.
pub trait ArtRenderer: Send {
    fn protocol(&self) -> ArtProtocol;
    fn render(&self, requests: &[RenderRequest]) -> Result<Vec<Vec<u8>>, RenderError>;
}

pub struct ChafaRenderer {
    binary: PathBuf,
    protocol: ArtProtocol,
}

impl ChafaRenderer {
    pub fn new(binary: impl Into<PathBuf>, protocol: ArtProtocol) -> Self {
        Self {
            binary: binary.into(),
            protocol,
        }
    }

    pub fn with_default_binary(protocol: ArtProtocol) -> Self {
        Self::new("chafa", protocol)
    }

    fn render_one(&self, request: &RenderRequest, format: &str) -> Result<Vec<u8>, RenderError> {
        let output = Command::new(&self.binary)
            .arg("-f")
            .arg(format)
            .arg("--size")
            .arg(format!("{}x{}", request.cols, request.rows))
            .arg("--colors")
            .arg(request.colors.to_string())
            .arg(&request.source)
            .output()
            .map_err(|err| RenderError::Io(format!("{}: {err}", self.binary.display())))?;
        if !output.status.success() {
            return Err(RenderError::Chafa(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }
        Ok(output.stdout)
    }
}

impl ArtRenderer for ChafaRenderer {
    fn protocol(&self) -> ArtProtocol {
        self.protocol
    }

    fn render(&self, requests: &[RenderRequest]) -> Result<Vec<Vec<u8>>, RenderError> {
        if requests.is_empty() || self.protocol == ArtProtocol::Off {
            return Ok(Vec::new());
        }

        // Only sixel is batched. chafa splits a kitty image across APC frames
        // with `m=1` continuations — twelve frames for a 2x2 render — so
        // splitting on the terminator would fragment one image into many.
        // kitty terminals are fast machines where one process per image is
        // free, so we do not batch them at all.
        if self.protocol != ArtProtocol::Sixel {
            let format = match self.protocol {
                ArtProtocol::Kitty => "kitty",
                _ => "symbols",
            };
            return requests
                .iter()
                .map(|request| {
                    self.render_one(request, format)
                        .map(|blob| trim_to_frame(blob, self.protocol))
                })
                .collect();
        }

        let shape = requests[0].batch_shape();
        if requests.iter().any(|r| r.batch_shape() != shape) {
            return Err(RenderError::MixedBatch);
        }

        let format = "sixel";

        let mut command = Command::new(&self.binary);
        command
            .arg("-f")
            .arg(format)
            .arg("--size")
            .arg(format!("{}x{}", shape.0, shape.1))
            .arg("--colors")
            .arg(shape.2.to_string());
        for request in requests {
            command.arg(&request.source);
        }

        let output = command
            .output()
            .map_err(|err| RenderError::Io(format!("{}: {err}", self.binary.display())))?;
        if !output.status.success() {
            return Err(RenderError::Chafa(
                String::from_utf8_lossy(&output.stderr).trim().to_string(),
            ));
        }

        let parts = split_sequences(&output.stdout, self.protocol);
        if parts.len() != requests.len() {
            // Misattributing covers to albums is worse than showing none.
            return Err(RenderError::SplitMismatch {
                expected: requests.len(),
                got: parts.len(),
            });
        }
        Ok(parts)
    }
}

/// Split batched sixel output into one blob per image.
///
/// sixel frames are `ESC P … ESC \`. Anything outside a frame (cursor-hide
/// sequences, newlines) is discarded. Non-sixel protocols pass through whole:
/// kitty frames are chunked and symbols are unframed, so neither can be split
/// back into images reliably.
pub fn split_sequences(output: &[u8], protocol: ArtProtocol) -> Vec<Vec<u8>> {
    let (start, end): (&[u8], &[u8]) = match protocol {
        ArtProtocol::Sixel => (b"\x1bP", b"\x1b\\"),
        ArtProtocol::Kitty | ArtProtocol::Symbols | ArtProtocol::Off => {
            return vec![output.to_vec()]
        }
    };

    let mut parts = Vec::new();
    let mut cursor = 0;
    while let Some(found) = find(&output[cursor..], start) {
        let frame_start = cursor + found;
        let after_start = frame_start + start.len();
        let Some(end_rel) = find(&output[after_start..], end) else {
            break;
        };
        let frame_end = after_start + end_rel + end.len();
        parts.push(output[frame_start..frame_end].to_vec());
        cursor = frame_end;
    }
    parts
}

/// Pixel dimensions from a sixel header: `ESC P … q " Pan ; Pad ; W ; H`.
///
/// The header is not quoted at the end — chafa emits `"1;1;30;40#0;…`, so the
/// height field runs into the first colour introducer and must be read as
/// leading digits only.
pub fn sixel_dimensions(blob: &[u8]) -> Option<(u32, u32)> {
    let window = &blob[..blob.len().min(128)];
    let text = std::str::from_utf8(window).ok()?;
    let quote = text.find('"')?;
    let mut fields = text[quote + 1..].split(';');
    let _pan = fields.next()?;
    let _pad = fields.next()?;
    let width = leading_u32(fields.next()?)?;
    let height = leading_u32(fields.next()?)?;
    Some((width, height))
}

/// Parse the leading decimal digits of a field, ignoring any trailing
/// sixel control characters.
fn leading_u32(field: &str) -> Option<u32> {
    let digits: String = field.chars().take_while(char::is_ascii_digit).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

/// chafa prefixes output with a cursor-hide sequence, assuming it owns the
/// terminal. In a TUI that would leave the cursor invisible, so drop anything
/// before the first kitty frame.
fn trim_to_frame(blob: Vec<u8>, protocol: ArtProtocol) -> Vec<u8> {
    if protocol == ArtProtocol::Kitty {
        if let Some(start) = find(&blob, b"\x1b_G") {
            return blob[start..].to_vec();
        }
    }
    blob
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SIXEL_BATCH: &[u8] = include_bytes!("../../tests/fixtures/chafa-batch-3.sixel");
    const KITTY_BATCH: &[u8] = include_bytes!("../../tests/fixtures/chafa-batch-2.kitty");
    const SYMBOLS: &[u8] = include_bytes!("../../tests/fixtures/chafa-single.symbols");
    const TEST_IMAGE: &str = "tests/fixtures/cover-test.jpg";

    #[test]
    fn splits_a_real_batched_sixel_output_into_three() {
        let parts = split_sequences(SIXEL_BATCH, ArtProtocol::Sixel);
        assert_eq!(parts.len(), 3);
        for part in &parts {
            assert!(part.starts_with(b"\x1bP"), "each part is a sixel frame");
            assert!(part.ends_with(b"\x1b\\"), "each part is terminated");
            assert!(sixel_dimensions(part).is_some());
        }
    }

    #[test]
    fn kitty_output_is_never_split() {
        // chafa chunks one kitty image across APC frames with `m=1`
        // continuations (12 frames for this 2x2 render). Splitting on the
        // terminator would fragment a single image into a dozen blobs, so
        // kitty output passes through whole.
        let parts = split_sequences(KITTY_BATCH, ArtProtocol::Kitty);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], KITTY_BATCH);
        assert!(KITTY_BATCH.iter().filter(|b| **b == 0x1b).count() > 2);
    }

    #[test]
    fn renders_kitty_one_image_per_process() {
        if !chafa_available() {
            eprintln!("skipping: chafa not on PATH");
            return;
        }
        let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Kitty);
        let requests = vec![
            RenderRequest::new(TEST_IMAGE, 2, 2, 16),
            RenderRequest::new(TEST_IMAGE, 2, 2, 16),
        ];
        let blobs = renderer.render(&requests).expect("render");
        assert_eq!(blobs.len(), 2, "two images, two processes");
        for blob in &blobs {
            assert!(blob.starts_with(b"\x1b_G"));
        }
    }

    #[test]
    fn splitting_discards_noise_outside_frames() {
        // chafa prefixes the first frame with a cursor-hide sequence.
        let parts = split_sequences(SIXEL_BATCH, ArtProtocol::Sixel);
        assert!(!parts[0].starts_with(b"\x1b[?25l"));
    }

    #[test]
    fn sixel_dimensions_are_read_from_the_header() {
        let parts = split_sequences(SIXEL_BATCH, ArtProtocol::Sixel);
        let (width, height) = sixel_dimensions(&parts[0]).expect("header");
        // chafa's cell box is 10x20 px, so a 3x2 request is a 30x40 box.
        assert_eq!(width, 30);
        assert!(height > 0);
    }

    #[test]
    fn empty_or_malformed_input_yields_no_frames() {
        assert!(split_sequences(b"", ArtProtocol::Sixel).is_empty());
        assert!(split_sequences(b"no frames here", ArtProtocol::Sixel).is_empty());
        // Unterminated frame.
        assert!(split_sequences(b"\x1bP something", ArtProtocol::Sixel).is_empty());
        assert_eq!(sixel_dimensions(b"not a sixel"), None);
    }

    #[test]
    fn symbols_passes_through_unsplit() {
        let parts = split_sequences(SYMBOLS, ArtProtocol::Symbols);
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0], SYMBOLS);
    }

    fn chafa_available() -> bool {
        std::process::Command::new("chafa")
            .arg("--version")
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[test]
    fn renders_a_batch_with_real_chafa() {
        if !chafa_available() {
            eprintln!("skipping: chafa not on PATH");
            return;
        }
        let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Sixel);
        let requests = vec![
            RenderRequest::new(TEST_IMAGE, 4, 3, 16),
            RenderRequest::new(TEST_IMAGE, 4, 3, 16),
        ];
        let blobs = renderer.render(&requests).expect("render");
        assert_eq!(blobs.len(), 2);
        for blob in &blobs {
            assert!(blob.starts_with(b"\x1bP"));
            assert!(sixel_dimensions(blob).is_some());
        }
    }

    #[test]
    fn real_chafa_respects_the_colour_budget() {
        if !chafa_available() {
            eprintln!("skipping: chafa not on PATH");
            return;
        }
        let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Sixel);
        let cheap = renderer
            .render(&[RenderRequest::new(TEST_IMAGE, 4, 3, 16)])
            .expect("16 colours");
        let rich = renderer
            .render(&[RenderRequest::new(TEST_IMAGE, 4, 3, 256)])
            .expect("256 colours");
        // Measured ratio on real art is roughly 8x; assert the direction and
        // that it is substantial, not an exact number.
        assert!(
            rich[0].len() > cheap[0].len() * 3,
            "256 colours ({}) should dwarf 16 colours ({})",
            rich[0].len(),
            cheap[0].len()
        );
    }

    #[test]
    fn mixed_sizes_are_refused() {
        let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Sixel);
        let requests = vec![
            RenderRequest::new(TEST_IMAGE, 4, 3, 16),
            RenderRequest::new(TEST_IMAGE, 8, 6, 16),
        ];
        assert!(matches!(
            renderer.render(&requests),
            Err(RenderError::MixedBatch)
        ));
    }

    #[test]
    fn off_protocol_renders_nothing() {
        let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Off);
        assert!(renderer
            .render(&[RenderRequest::new(TEST_IMAGE, 4, 3, 16)])
            .unwrap()
            .is_empty());
    }

    #[test]
    fn missing_source_is_an_error_not_a_panic() {
        let renderer = ChafaRenderer::with_default_binary(ArtProtocol::Sixel);
        let result = renderer.render(&[RenderRequest::new("/nonexistent/nope.jpg", 4, 3, 16)]);
        assert!(result.is_err());
    }

    #[test]
    fn path_type_is_used_for_sources() {
        let request = RenderRequest::new(std::path::Path::new("x.jpg"), 1, 1, 16);
        assert_eq!(request.source, PathBuf::from("x.jpg"));
    }
}
