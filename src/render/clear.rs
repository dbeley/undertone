//! Removing art that is already on screen.
//!
//! Blanking the character cells an image occupies does **not** remove its
//! pixels: sixel rasterises into the terminal's image layer, and kitty graphics
//! are objects that float above the grid. So a scrolled row kept showing the
//! previous album's cover under its placeholder, which reads as "wrong cover"
//! rather than "not loaded yet".
//!
//! Two mechanisms, one per protocol:
//!
//! * sixel has no delete, so the rectangle is overpainted with a solid image of
//!   the same size in the configured backdrop colour;
//! * kitty can delete by image id, but chafa does not set one, so ids are
//!   injected into the blob when it is cached and the same id is deleted later.

/// A solid sixel covering `width` x `height` pixels.
///
/// Painted instead of deleted: sixel has no delete operation, and a full-rect
/// opaque image is both the cheapest and the most portable way to clear one.
///
/// The size is in pixels, not cells, because the image being covered was
/// sized in pixels too: a blank of the wrong size leaves a rim of the old
/// cover behind.
pub fn blank_sixel(width: u32, height: u32, rgb: [u8; 3]) -> Vec<u8> {
    let width = width.max(1);
    let height = height.max(1);
    let bands = height.div_ceil(6);

    let mut out = Vec::with_capacity(64 + bands as usize * 8);
    out.extend_from_slice(b"\x1bP0;1;0q");
    out.extend_from_slice(format!("\"1;1;{width};{height}").as_bytes());
    out.extend_from_slice(
        format!(
            "#0;2;{};{};{}",
            percent(rgb[0]),
            percent(rgb[1]),
            percent(rgb[2])
        )
        .as_bytes(),
    );
    out.extend_from_slice(b"#0");
    for band in 0..bands {
        if band > 0 {
            out.push(b'-');
        }
        // `!N~` is N repetitions of "all six pixels lit".
        out.extend_from_slice(format!("!{width}~").as_bytes());
    }
    out.extend_from_slice(b"\x1b\\");
    out
}

fn percent(value: u8) -> u32 {
    (u32::from(value) * 100 + 127) / 255
}

/// Stable non-zero image id for a cache key.
///
/// FNV-1a, because the id only has to be unique within this process; it is
/// derived from the key so an erase can name the image it wants gone without
/// carrying any extra state.
pub fn kitty_id(key: &str) -> u32 {
    let mut hash: u32 = 2_166_136_261;
    for byte in key.as_bytes() {
        hash ^= u32::from(*byte);
        hash = hash.wrapping_mul(16_777_619);
    }
    if hash == 0 {
        1
    } else {
        hash
    }
}

/// Add `i=<id>` to every APC chunk of a kitty blob.
///
/// Continuation chunks carry the same parameters, and the terminal keys them on
/// the id, so every chunk gets it. Already-identified blobs are left alone.
pub fn tag_kitty(blob: &[u8], id: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity(blob.len() + 64);
    let mut index = 0;
    while index < blob.len() {
        // An APC introducer is ESC _ G; anything else is copied verbatim.
        if blob[index..].starts_with(b"\x1b_G") {
            out.extend_from_slice(b"\x1b_G");
            let rest = &blob[index + 3..];
            if !rest.starts_with(b"i=") {
                out.extend_from_slice(format!("i={id},").as_bytes());
            }
            index += 3;
            continue;
        }
        out.push(blob[index]);
        index += 1;
    }
    out
}

/// Delete a kitty image by id.
pub fn kitty_delete(id: u32) -> Vec<u8> {
    format!("\x1b_Ga=d,d=i,i={id}\x1b\\").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_blank_sixel_is_well_formed_and_covers_the_rectangle() {
        let bytes = blank_sixel(40, 40, [0, 0, 0]);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.starts_with("\x1bP0;1;0q"));
        assert!(text.ends_with("\x1b\\"));
        assert!(text.contains("\"1;1;40;40"), "{text}");
        // Forty pixels tall is seven bands of six.
        assert_eq!(text.matches('~').count(), 7);
        assert_eq!(
            text.matches('-').count(),
            6,
            "one separator fewer than bands"
        );
    }

    #[test]
    fn backdrop_colour_is_encoded_as_sixel_percentages() {
        let white = String::from_utf8_lossy(&blank_sixel(10, 20, [255, 255, 255])).to_string();
        assert!(white.contains("#0;2;100;100;100"));
        let mid = String::from_utf8_lossy(&blank_sixel(10, 20, [128, 0, 0])).to_string();
        assert!(mid.contains("#0;2;50;0;0"), "{mid}");
    }

    #[test]
    fn kitty_ids_are_stable_and_never_zero() {
        assert_eq!(kitty_id("al-1/4x2-16-sixel"), kitty_id("al-1/4x2-16-sixel"));
        assert_ne!(kitty_id("al-1"), kitty_id("al-2"));
        assert_ne!(kitty_id(""), 0);
    }

    #[test]
    fn tagging_adds_the_id_to_every_chunk_and_is_idempotent() {
        let blob = b"\x1b_Ga=T,f=32,m=1,q=2\x1b\\\x1b_Gm=1;AAAA\x1b\\";
        let tagged = tag_kitty(blob, 42);
        let text = String::from_utf8_lossy(&tagged);
        assert_eq!(text.matches("i=42,").count(), 2, "{text}");
        assert!(text.contains("\x1b_Gi=42,a=T"));
        assert!(text.contains("\x1b_Gi=42,m=1;"));

        let again = tag_kitty(&tagged, 42);
        assert_eq!(again, tagged, "tagging twice must not double the id");
    }

    #[test]
    fn tagging_leaves_non_kitty_bytes_alone() {
        let blob = b"\x1bP0;1;0qdata\x1b\\";
        assert_eq!(tag_kitty(blob, 7), blob);
    }

    #[test]
    fn delete_names_the_id() {
        assert_eq!(
            String::from_utf8_lossy(&kitty_delete(99)),
            "\x1b_Ga=d,d=i,i=99\x1b\\"
        );
    }
}
