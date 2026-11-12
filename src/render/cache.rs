//! On-disk cache of encoded escape blobs.
//!
//! A cache hit must be a file read plus an escape write — never a re-encode.
//! Blobs are keyed by `(cover id, pixel size, colours, protocol)`, written
//! atomically, and evicted least-recently-modified first.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

pub struct BlobCache {
    root: PathBuf,
    capacity_bytes: u64,
    used_bytes: u64,
    /// Most recently written key. Eviction never reclaims it: filesystem
    /// timestamps have coarse granularity, so a blob written microseconds ago
    /// can look exactly as old as its neighbours, and dropping the entry the
    /// caller is about to display is the one eviction we must not make.
    newest_key: Option<String>,
}

/// Cheap check that a cached blob ends the way a finished escape sequence does.
///
/// chafa terminates sixel and kitty payloads with ST (`ESC \\`) and appends a
/// cursor-show (`ESC [ ? 25 h`), so either tail counts. A truncated file loses
/// both.
fn looks_complete(blob: &[u8]) -> bool {
    const ENDINGS: [&[u8]; 3] = [b"\x1b\\", b"\x1b[?25h", b"\x9c"];
    if blob.len() < 4 || blob[0] != 0x1b {
        return false;
    }
    let trimmed = blob.trim_ascii_end();
    ENDINGS.iter().any(|ending| trimmed.ends_with(ending))
}

impl BlobCache {
    /// Open (creating if needed) a cache directory with a byte budget.
    pub fn new(root: impl Into<PathBuf>, capacity_bytes: u64) -> io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        let mut cache = Self {
            root,
            capacity_bytes,
            used_bytes: 0,
            newest_key: None,
        };
        cache.used_bytes = cache.scan_used_bytes()?;
        Ok(cache)
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    pub fn capacity_bytes(&self) -> u64 {
        self.capacity_bytes
    }

    pub fn path_for(&self, key: &str) -> PathBuf {
        self.root.join(key_to_filename(key))
    }

    /// Read a blob. Returns `None` on a miss, an unreadable file, or a blob
    /// that does not look finished.
    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        let blob = fs::read(self.path_for(key)).ok()?;
        // Writes are not fsynced (see `put`), so a power cut can leave a
        // truncated file behind. Reject it here rather than handing half a
        // sixel to the terminal: it is re-rendered instead, which is cheap,
        // whereas a corrupt blob is visible and permanent.
        if looks_complete(&blob) {
            Some(blob)
        } else {
            None
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        self.path_for(key).is_file()
    }

    /// Store a blob atomically, then evict if over budget.
    pub fn put(&mut self, key: &str, blob: &[u8]) -> io::Result<()> {
        let final_path = self.path_for(key);
        let previous = fs::metadata(&final_path).map(|m| m.len()).unwrap_or(0);

        // Write to a unique temporary file in the same directory, then rename:
        // a reader never sees a half-written blob.
        let temp_path = self.root.join(format!(
            ".tmp-{}-{:x}",
            std::process::id(),
            monotonic_nanos()
        ));
        {
            let mut file = fs::File::create(&temp_path)?;
            file.write_all(blob)?;
            // Deliberately no `sync_all`: this is a cache, and a durable write
            // per cover costs about 7 ms on this machine — far more on an SD
            // card, which is exactly the hardware this targets. The rename is
            // still atomic, and `get` rejects a truncated blob, so the worst
            // case after a power cut is re-rendering one image.
        }
        fs::rename(&temp_path, &final_path)?;

        self.used_bytes = self.used_bytes.saturating_sub(previous) + blob.len() as u64;
        self.newest_key = Some(key.to_string());
        self.evict_to_capacity()?;
        Ok(())
    }

    /// Delete oldest blobs until the cache fits its budget.
    pub fn evict_to_capacity(&mut self) -> io::Result<()> {
        if self.used_bytes <= self.capacity_bytes {
            return Ok(());
        }
        let mut entries = self.entries()?;
        // Oldest first.
        entries.sort_by(|a, b| a.1.cmp(&b.1).then_with(|| a.0.cmp(&b.0)));

        let protected = self.newest_key.as_deref().map(|key| self.path_for(key));
        for (path, _) in entries {
            if self.used_bytes <= self.capacity_bytes {
                break;
            }
            if protected.as_deref() == Some(path.as_path()) {
                continue;
            }
            let size = fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if fs::remove_file(&path).is_ok() {
                self.used_bytes = self.used_bytes.saturating_sub(size);
            }
        }
        Ok(())
    }

    /// Remove everything. Used by tests and by a `--clear-cache` flag.
    pub fn clear(&mut self) -> io::Result<()> {
        for (path, _) in self.entries()? {
            let _ = fs::remove_file(path);
        }
        self.used_bytes = 0;
        self.newest_key = None;
        Ok(())
    }

    fn scan_used_bytes(&self) -> io::Result<u64> {
        Ok(self
            .entries()?
            .iter()
            .map(|(path, _)| fs::metadata(path).map(|m| m.len()).unwrap_or(0))
            .sum())
    }

    /// `(path, modified)` for every cached blob.
    fn entries(&self) -> io::Result<Vec<(PathBuf, SystemTime)>> {
        let mut out = Vec::new();
        let reader = match fs::read_dir(&self.root) {
            Ok(reader) => reader,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(out),
            Err(err) => return Err(err),
        };
        for entry in reader {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("blob") {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            out.push((path, modified));
        }
        Ok(out)
    }
}

/// Keys may contain `/` and `:`; filenames may not.
///
/// Escaping is percent-style and reversible, so distinct keys can never
/// collide (`a/b` becomes `a%2Fb`, which is not `a_b`).
pub fn key_to_filename(key: &str) -> String {
    let mut name = String::with_capacity(key.len() + 8);
    for byte in key.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' => {
                name.push(byte as char);
            }
            _ => {
                use std::fmt::Write as _;
                let _ = write!(name, "%{byte:02X}");
            }
        }
    }
    if name.len() > 120 {
        // Long keys get a stable hash rather than a truncation that could
        // collide with a sibling.
        name = format!("h-{:016x}", fnv1a(key));
    }
    name.push_str(".blob");
    name
}

fn fnv1a(value: &str) -> u64 {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in value.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    hash
}

fn monotonic_nanos() -> u128 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "undertone-cache-{}-{}-{}",
            std::process::id(),
            tag,
            monotonic_nanos()
        ));
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn round_trips_a_blob() {
        let mut cache = BlobCache::new(temp_root("roundtrip"), 1024).unwrap();
        let blob = b"\x1bP1;1;40;40#0;2;0;0;0#0!40~\x1b\\";
        cache.put("album/1/40x40-16-sixel", blob).unwrap();
        assert_eq!(
            cache.get("album/1/40x40-16-sixel").as_deref(),
            Some(&blob[..])
        );
        assert!(cache.contains("album/1/40x40-16-sixel"));
    }

    #[test]
    fn a_truncated_blob_is_a_miss_not_a_corrupt_image() {
        let mut cache = BlobCache::new(temp_root("truncated"), 1024).unwrap();
        let blob = b"\x1bP1;1;40;40#0;2;0;0;0#0!40~\x1b[?25h";
        cache.put("k", blob).unwrap();
        assert!(cache.get("k").is_some());

        // Simulate the tail being lost to a power cut.
        std::fs::write(cache.path_for("k"), &blob[..blob.len() - 20]).unwrap();
        assert!(
            cache.get("k").is_none(),
            "a half-written blob must be re-rendered, not painted"
        );
    }

    #[test]
    fn junk_is_never_returned_as_a_blob() {
        let cache = BlobCache::new(temp_root("junk"), 1024).unwrap();
        std::fs::write(cache.path_for("k"), b"not an image at all").unwrap();
        assert!(cache.get("k").is_none());
    }

    #[test]
    fn missing_keys_are_none() {
        let cache = BlobCache::new(temp_root("missing"), 1024).unwrap();
        assert_eq!(cache.get("nope"), None);
        assert!(!cache.contains("nope"));
    }

    #[test]
    fn overwriting_replaces_and_reaccounts() {
        let mut cache = BlobCache::new(temp_root("overwrite"), 1024).unwrap();
        cache.put("k", b"\x1bP0123456789\x1b\\").unwrap();
        assert_eq!(cache.used_bytes(), 14);
        cache.put("k", b"\x1bPabc\x1b\\").unwrap();
        assert_eq!(cache.used_bytes(), 7);
        assert_eq!(cache.get("k").as_deref(), Some(&b"\x1bPabc\x1b\\"[..]));
    }

    #[test]
    fn evicts_oldest_until_within_budget() {
        let mut cache = BlobCache::new(temp_root("evict"), 100).unwrap();
        cache.put("first", &[1u8; 60]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.put("second", &[2u8; 60]).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        cache.put("third", &[3u8; 60]).unwrap();

        assert!(cache.used_bytes() <= 100, "used {}", cache.used_bytes());
        assert!(cache.contains("third"), "newest entry must survive");
        assert!(!cache.contains("first"), "oldest entry must be evicted");
    }

    #[test]
    fn a_large_blob_evicts_everything_else() {
        let mut cache = BlobCache::new(temp_root("bigblob"), 100).unwrap();
        cache.put("small", &[1u8; 40]).unwrap();
        cache.put("huge", &[2u8; 90]).unwrap();
        assert!(cache.contains("huge"));
        assert!(cache.used_bytes() <= 100);
    }

    #[test]
    fn the_newest_blob_survives_even_when_timestamps_tie() {
        // Filesystem timestamps are coarse enough that two writes can land in
        // the same tick, at which point sorting by mtime makes the order
        // arbitrary. The entry just written is the one the caller is about to
        // paint, so it must never be the victim.
        let mut cache = BlobCache::new(temp_root("tiebreak"), 100).unwrap();
        for i in 0..20 {
            let key = format!("k{i:02}");
            cache.put(&key, &[7u8; 60]).unwrap();
            assert!(
                cache.contains(&key),
                "{key} was evicted right after being written"
            );
            assert!(cache.used_bytes() <= 100, "over budget after {key}");
        }
    }

    #[test]
    fn a_blob_larger_than_the_budget_is_kept_rather_than_thrown_away() {
        // Better to exceed the budget until the next write than to discard a
        // blob we just spent a fetch and a chafa run producing.
        let mut cache = BlobCache::new(temp_root("overbudget"), 100).unwrap();
        cache.put("enormous", &[3u8; 400]).unwrap();
        assert!(cache.contains("enormous"));
        assert_eq!(cache.used_bytes(), 400);

        // The next write reclaims it.
        cache.put("next", &[4u8; 10]).unwrap();
        assert!(!cache.contains("enormous"));
    }

    #[test]
    fn keys_with_separators_are_sanitised() {
        let name = key_to_filename("al-abc_123/40x40-16-sixel");
        assert!(!name.contains('/'));
        assert!(name.ends_with(".blob"));
        // Distinct keys must not collide into the same file.
        assert_ne!(key_to_filename("a/b"), key_to_filename("a_b"));
    }

    #[test]
    fn very_long_keys_use_a_hash() {
        let long = "x".repeat(400);
        let name = key_to_filename(&long);
        assert!(name.len() < 32);
        assert!(name.ends_with(".blob"));
        assert_ne!(name, key_to_filename(&"x".repeat(401)));
    }

    #[test]
    fn reopening_recomputes_usage() {
        let root = temp_root("reopen");
        {
            let mut cache = BlobCache::new(&root, 1024).unwrap();
            cache.put("a", &[0u8; 10]).unwrap();
            cache.put("b", &[0u8; 20]).unwrap();
        }
        let cache = BlobCache::new(&root, 1024).unwrap();
        assert_eq!(cache.used_bytes(), 30);
    }

    #[test]
    fn clear_empties_the_cache() {
        let mut cache = BlobCache::new(temp_root("clear"), 1024).unwrap();
        cache.put("a", b"hello").unwrap();
        cache.clear().unwrap();
        assert_eq!(cache.used_bytes(), 0);
        assert!(cache.get("a").is_none());
    }

    #[test]
    fn temp_files_are_not_counted_as_blobs() {
        let mut cache = BlobCache::new(temp_root("tempfiles"), 1024).unwrap();
        cache.put("a", b"hello").unwrap();
        // Simulate a crashed writer.
        fs::write(cache.root().join(".tmp-999-dead"), b"garbage").unwrap();
        let reopened = BlobCache::new(cache.root(), 1024).unwrap();
        assert_eq!(reopened.used_bytes(), 5);
    }
}
