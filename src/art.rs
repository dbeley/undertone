//! Art pipeline facade for the UI.
//!
//! The UI asks for a thumbnail by cover id and gets a cache key back
//! immediately. Blobs arrive later through `poll` and are then drawn by the
//! painter. Nothing here blocks.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Arc;

use crate::render::{
    ArtProtocol, BlobCache, ChafaRenderer, ImageCache, ImageSource, Job, RenderWorker,
};
use crate::subsonic::SubsonicClient;

/// Fetches cover art through the Subsonic client, pre-scaled by the server.
pub struct SubsonicImageSource {
    client: SubsonicClient,
    size: u32,
}

impl SubsonicImageSource {
    pub fn new(client: SubsonicClient, size: u32) -> Self {
        Self { client, size }
    }
}

impl ImageSource for SubsonicImageSource {
    fn fetch(&self, cover_id: &str) -> Result<Vec<u8>, String> {
        self.client
            .cover_art(cover_id, Some(self.size))
            .map(|(bytes, _content_type)| bytes)
            .map_err(|err| err.to_string())
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct ArtStats {
    pub rendered: usize,
    pub cached: usize,
    pub failed_requests: usize,
}

pub struct ArtService {
    worker: RenderWorker,
    protocol: ArtProtocol,
    blobs: HashMap<String, Vec<u8>>,
    requested: HashSet<String>,
    failed: HashSet<String>,
    stats: ArtStats,
}

impl ArtService {
    pub fn new(worker: RenderWorker, protocol: ArtProtocol) -> Self {
        Self {
            worker,
            protocol,
            blobs: HashMap::new(),
            requested: HashSet::new(),
            failed: HashSet::new(),
            stats: ArtStats::default(),
        }
    }

    /// Build the real pipeline: chafa + disk caches + Subsonic source.
    pub fn from_settings(
        protocol: ArtProtocol,
        cache_dir: &Path,
        cache_bytes: u64,
        source: Option<Arc<dyn ImageSource>>,
    ) -> std::io::Result<Self> {
        let blobs = BlobCache::new(cache_dir.join("blobs"), cache_bytes)?;
        let images = ImageCache::new(cache_dir.join("images"))?;
        let renderer = Box::new(ChafaRenderer::with_default_binary(protocol));
        let worker = RenderWorker::new(renderer, blobs, images, source);
        Ok(Self::new(worker, protocol))
    }

    pub fn protocol(&self) -> ArtProtocol {
        self.protocol
    }

    pub fn stats(&self) -> ArtStats {
        self.stats
    }

    /// Cache key for one rendering of a cover at one size and colour budget.
    pub fn key(cover_id: &str, cols: u32, rows: u32, colors: u32, protocol: ArtProtocol) -> String {
        format!("{cover_id}/{cols}x{rows}-{colors}-{}", protocol.label())
    }

    /// Request art, returning its cache key. Cheap and non-blocking.
    pub fn request(
        &mut self,
        cover_id: &str,
        cols: u32,
        rows: u32,
        colors: u32,
        prefetch: bool,
    ) -> String {
        let key = Self::key(cover_id, cols, rows, colors, self.protocol);
        if self.blobs.contains_key(&key)
            || self.requested.contains(&key)
            || self.failed.contains(&key)
            || !self.protocol.is_pixel()
        {
            return key;
        }
        let job = if prefetch {
            Job::prefetch(key.clone(), cover_id, cols, rows, colors)
        } else {
            Job::visible(key.clone(), cover_id, cols, rows, colors)
        };
        self.worker.submit(job);
        self.requested.insert(key.clone());
        key
    }

    /// A rendered blob, if it has arrived.
    pub fn blob(&self, key: &str) -> Option<&[u8]> {
        self.blobs.get(key).map(Vec::as_slice)
    }

    pub fn is_ready(&self, key: &str) -> bool {
        self.blobs.contains_key(key)
    }

    /// Collect finished renders. Returns true when anything changed, so the
    /// caller knows to redraw.
    pub fn poll(&mut self) -> bool {
        let done = self.worker.drain_done();
        if done.is_empty() {
            return false;
        }
        for item in done {
            self.requested.remove(&item.key);
            if item.from_cache {
                self.stats.cached += 1;
            } else {
                self.stats.rendered += 1;
            }
            self.blobs.insert(item.key, item.blob);
        }
        true
    }

    /// Drop requests that are no longer visible.
    pub fn cancel(&self, keys: &[String]) {
        self.worker.cancel(keys);
    }

    pub fn pending(&self) -> usize {
        self.worker.pending()
    }

    pub fn shutdown(&mut self) {
        self.worker.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{ArtRenderer, RenderError, RenderRequest};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct FakeRenderer {
        calls: Arc<AtomicUsize>,
    }

    impl ArtRenderer for FakeRenderer {
        fn protocol(&self) -> ArtProtocol {
            ArtProtocol::Sixel
        }

        fn render(&self, requests: &[RenderRequest]) -> Result<Vec<Vec<u8>>, RenderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(requests
                .iter()
                .map(|r| format!("blob-{}", r.cols).into_bytes())
                .collect())
        }
    }

    struct NoSource;
    impl ImageSource for NoSource {
        fn fetch(&self, _: &str) -> Result<Vec<u8>, String> {
            Ok(b"x".to_vec())
        }
    }

    fn service(tag: &str) -> (ArtService, Arc<AtomicUsize>) {
        let calls = Arc::new(AtomicUsize::new(0));
        let base = std::env::temp_dir().join(format!(
            "undertone-art-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let blobs = BlobCache::new(base.join("blobs"), 1024 * 1024).unwrap();
        let images = ImageCache::new(base.join("img")).unwrap();
        let renderer = FakeRenderer {
            calls: Arc::clone(&calls),
        };
        let worker = RenderWorker::new(Box::new(renderer), blobs, images, Some(Arc::new(NoSource)));
        (ArtService::new(worker, ArtProtocol::Sixel), calls)
    }

    #[test]
    fn keys_include_size_and_protocol() {
        let a = ArtService::key("al-1", 4, 3, 16, ArtProtocol::Sixel);
        let b = ArtService::key("al-1", 4, 3, 16, ArtProtocol::Kitty);
        let c = ArtService::key("al-1", 8, 3, 16, ArtProtocol::Sixel);
        assert_ne!(a, b);
        assert_ne!(a, c);
    }

    #[test]
    fn requesting_twice_queues_once() {
        let (mut service, calls) = service("once");
        let key = service.request("al-1", 4, 3, 16, false);
        let again = service.request("al-1", 4, 3, 16, false);
        assert_eq!(key, again);
        assert_eq!(service.pending(), 1);
        let _ = calls;
        service.shutdown();
    }

    #[test]
    fn a_blob_becomes_available_after_poll() {
        let (mut service, _) = service("poll");
        let key = service.request("al-1", 4, 3, 16, false);
        assert!(service.blob(&key).is_none());

        let deadline = Instant::now() + Duration::from_secs(5);
        while Instant::now() < deadline && !service.is_ready(&key) {
            service.poll();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(service.is_ready(&key));
        assert_eq!(service.blob(&key), Some(&b"blob-4"[..]));
        service.shutdown();
    }

    #[test]
    fn symbols_mode_never_requests_rendering() {
        let (service, _) = service("symbols");
        let mut service = ArtService {
            protocol: ArtProtocol::Symbols,
            ..service
        };
        let key = service.request("al-1", 4, 3, 16, false);
        assert!(key.contains("symbols"));
        assert_eq!(service.pending(), 0);
        service.shutdown();
    }
}
