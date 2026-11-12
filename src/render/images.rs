//! Raw cover-art cache.
//!
//! chafa needs a file path, so fetched image bytes are written here first and
//! the render worker then encodes from disk. This is deliberately separate
//! from the escape-blob cache: one holds images, the other holds what the
//! terminal eats.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

pub struct ImageCache {
    root: PathBuf,
}

impl ImageCache {
    pub fn new(root: impl Into<PathBuf>) -> std::io::Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, key: &str) -> PathBuf {
        // Extension does not matter to chafa, which sniffs content.
        self.root.join(format!("{}.img", sanitize(key)))
    }

    pub fn contains(&self, key: &str) -> bool {
        self.path_for(key).is_file()
    }

    /// Store bytes atomically and return the path to hand to chafa.
    pub fn store(&self, key: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
        let final_path = self.path_for(key);
        let temp = self
            .root
            .join(format!(".tmp-{}-{:x}", std::process::id(), nanos()));
        {
            let mut file = fs::File::create(&temp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
        }
        fs::rename(&temp, &final_path)?;
        Ok(final_path)
    }

    pub fn get(&self, key: &str) -> Option<Vec<u8>> {
        fs::read(self.path_for(key)).ok()
    }
}

fn sanitize(key: &str) -> String {
    key.bytes()
        .map(|byte| match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' => byte as char,
            _ => '%',
        })
        .collect()
}

fn nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "undertone-images-{}-{}-{}",
            std::process::id(),
            tag,
            nanos()
        ))
    }

    #[test]
    fn stores_and_reads_back() {
        let cache = ImageCache::new(temp_root("basic")).unwrap();
        let path = cache.store("al-abc_123", b"not really a jpeg").unwrap();
        assert!(path.is_file());
        assert!(cache.contains("al-abc_123"));
        assert_eq!(
            cache.get("al-abc_123").as_deref(),
            Some(&b"not really a jpeg"[..])
        );
    }

    #[test]
    fn missing_images_are_none() {
        let cache = ImageCache::new(temp_root("missing")).unwrap();
        assert!(!cache.contains("nope"));
        assert!(cache.get("nope").is_none());
    }

    #[test]
    fn keys_cannot_escape_the_cache_directory() {
        let cache = ImageCache::new(temp_root("escape")).unwrap();
        let path = cache.store("al-abc_123", b"x").unwrap();
        assert!(path.starts_with(cache.root()));
        // Separators are flattened, so a traversal attempt stays inside.
        let evil = cache.path_for("../../etc/passwd");
        assert!(evil.starts_with(cache.root()));
        assert!(!evil.to_string_lossy().contains(".."));
    }
}
