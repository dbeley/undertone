//! Background art rendering.
//!
//! One thread does everything expensive: fetching cover bytes, writing them to
//! the image cache, encoding with chafa, and storing the escape blob. Nothing
//! on the input path ever touches the network or spawns a process — that is
//! what keeps a TUI responsive on weak hardware.
//!
//! Jobs carry a priority so what is visible now beats what is being prefetched
//! for the next screen, and cancelled jobs are dropped before they cost a
//! network round trip or a process spawn.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::cache::BlobCache;
use super::chafa::{sixel_dimensions, ArtRenderer, RenderRequest};
use super::images::ImageCache;
use super::protocol::ArtProtocol;

/// Upper bound on images per chafa invocation.
///
/// Kept equal to the fetch concurrency on purpose: a batch is only rendered
/// once every image in it has arrived, so a large batch means one slow cover
/// delays the whole screenful. Four keeps chafa spawns cheap while bounding
/// how long a stall can hold back the first paint.
pub const MAX_BATCH: usize = 4;

/// Pixels fetched from the server, once per cover.
///
/// One size for every view: a detail cover is 20x8 cells = 200x160 px, which
/// is the largest thing we draw. Fetching per-view sizes meant switching
/// layouts refetched every cover; one size keeps a single image per album and
/// lets chafa downscale for the smaller renders.
pub const IMAGE_PX: u32 = 200;

/// How many covers to fetch at once. Encoding stays single-threaded; network
/// wait is what benefits from concurrency.
pub const FETCH_CONCURRENCY: usize = 4;

/// Priority for art the user can see right now.
pub const PRIORITY_VISIBLE: u8 = 0;
/// Priority for art that will scroll into view soon.
pub const PRIORITY_PREFETCH: u8 = 10;

/// Fetches cover-art bytes by cover id. Implemented over the Subsonic client
/// in `art`, and faked in tests.
pub trait ImageSource: Send + Sync {
    /// Fetch cover bytes, pre-scaled to at most `px` pixels on the long edge.
    fn fetch(&self, cover_id: &str, px: u32) -> Result<Vec<u8>, String>;
}

#[derive(Debug, Clone)]
pub struct Job {
    /// Cache key, built by the caller.
    pub key: String,
    /// Subsonic `coverArt` token — not the album id.
    pub cover_id: String,
    pub cols: u32,
    pub rows: u32,
    pub colors: u32,
    pub priority: u8,
}

impl Job {
    pub fn visible(
        key: impl Into<String>,
        cover_id: impl Into<String>,
        cols: u32,
        rows: u32,
        colors: u32,
    ) -> Self {
        Self {
            key: key.into(),
            cover_id: cover_id.into(),
            cols,
            rows,
            colors,
            priority: PRIORITY_VISIBLE,
        }
    }

    pub fn prefetch(
        key: impl Into<String>,
        cover_id: impl Into<String>,
        cols: u32,
        rows: u32,
        colors: u32,
    ) -> Self {
        Self {
            priority: PRIORITY_PREFETCH,
            ..Self::visible(key, cover_id, cols, rows, colors)
        }
    }

    fn shape(&self) -> (u32, u32, u32) {
        (self.cols, self.rows, self.colors)
    }

    /// The image we need, in pixels.
    ///
    /// chafa's sixel geometry is a fixed 10x20 px cell, so this is the pixel
    /// box the encoder fills. One size is cached per cover and reused by every
    /// view, so changing layout or opening an album costs no refetch.
    fn px(&self) -> u32 {
        IMAGE_PX
    }

    /// Image-cache key: one cached image per cover, independent of view.
    fn image_key(&self) -> String {
        self.cover_id.clone()
    }
}

#[derive(Debug, Clone)]
pub struct Done {
    pub key: String,
    pub blob: Vec<u8>,
    /// Actual pixel dimensions reported by the protocol, when known.
    pub dimensions: Option<(u32, u32)>,
    pub from_cache: bool,
}

#[derive(Default)]
struct Shared {
    queue: Vec<Job>,
    cancelled: HashSet<String>,
    shutdown: bool,
}

pub struct RenderWorker {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    done_rx: Receiver<Done>,
    handle: Option<JoinHandle<()>>,
}

impl RenderWorker {
    pub fn new(
        renderer: Box<dyn ArtRenderer>,
        blobs: BlobCache,
        images: ImageCache,
        source: Option<Arc<dyn ImageSource>>,
    ) -> Self {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let (done_tx, done_rx) = mpsc::channel();

        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("art-render".to_string())
            .spawn(move || worker_loop(thread_shared, renderer, blobs, images, source, done_tx))
            .expect("spawn render worker");

        Self {
            shared,
            done_rx,
            handle: Some(handle),
        }
    }

    pub fn submit(&self, job: Job) {
        let (lock, cvar) = &*self.shared;
        let mut shared = lock.lock().expect("render queue poisoned");
        if shared.shutdown {
            return;
        }
        // Coalesce a duplicate that is already queued.
        if shared
            .queue
            .iter()
            .any(|queued| queued.key == job.key && queued.priority == job.priority)
        {
            return;
        }
        shared.queue.push(job);
        cvar.notify_one();
    }

    pub fn submit_all(&self, jobs: impl IntoIterator<Item = Job>) {
        for job in jobs {
            self.submit(job);
        }
    }

    /// Drop queued and in-flight work for these keys.
    pub fn cancel(&self, keys: &[String]) {
        if keys.is_empty() {
            return;
        }
        let (lock, _) = &*self.shared;
        let mut shared = lock.lock().expect("render queue poisoned");
        let set: HashSet<&String> = keys.iter().collect();
        shared.queue.retain(|job| !set.contains(&job.key));
        shared.cancelled.extend(keys.iter().cloned());
    }

    pub fn pending(&self) -> usize {
        let (lock, _) = &*self.shared;
        lock.lock().expect("render queue poisoned").queue.len()
    }

    /// Take every finished render since the last call. Never blocks.
    pub fn drain_done(&self) -> Vec<Done> {
        let mut done = Vec::new();
        while let Ok(item) = self.done_rx.try_recv() {
            done.push(item);
        }
        done
    }

    pub fn shutdown(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut shared = lock.lock().expect("render queue poisoned");
            shared.shutdown = true;
            shared.queue.clear();
            cvar.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for RenderWorker {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn worker_loop(
    shared: Arc<(Mutex<Shared>, Condvar)>,
    renderer: Box<dyn ArtRenderer>,
    mut blobs: BlobCache,
    images: ImageCache,
    source: Option<Arc<dyn ImageSource>>,
    done_tx: Sender<Done>,
) {
    while let Some(batch) = take_batch(&shared) {
        let batch = filter_cancelled(&shared, batch);
        if batch.is_empty() {
            continue;
        }

        // Serve blob-cache hits immediately; everything else needs work.
        let mut pending: Vec<Job> = Vec::new();
        for job in batch {
            if let Some(blob) = blobs.get(&job.key) {
                let dimensions = dimensions_of(renderer.protocol(), &blob);
                if done_tx
                    .send(Done {
                        key: job.key,
                        blob,
                        dimensions,
                        from_cache: true,
                    })
                    .is_err()
                {
                    return;
                }
                continue;
            }
            pending.push(job);
        }
        if pending.is_empty() {
            continue;
        }

        // Fetch missing images concurrently. Serially fetching one cover at a
        // time was the bottleneck on a fast machine, not encoding.
        if let Some(source) = source.as_ref() {
            let mut to_fetch: Vec<(String, u32)> = Vec::new();
            for job in &pending {
                if images.contains(&job.image_key()) {
                    continue;
                }
                let entry = (job.cover_id.clone(), job.px());
                if !to_fetch.contains(&entry) {
                    to_fetch.push(entry);
                }
            }

            for chunk in to_fetch.chunks(FETCH_CONCURRENCY) {
                let results: Vec<Result<Vec<u8>, String>> = std::thread::scope(|scope| {
                    let handles: Vec<_> = chunk
                        .iter()
                        .map(|(cover, px)| {
                            let source = source.as_ref();
                            scope.spawn(move || source.fetch(cover, *px))
                        })
                        .collect();
                    handles
                        .into_iter()
                        .map(|handle| {
                            handle
                                .join()
                                .unwrap_or_else(|_| Err("fetch thread panicked".into()))
                        })
                        .collect()
                });

                for ((cover, _px), result) in chunk.iter().zip(results) {
                    match result {
                        Ok(bytes) => {
                            // Image key is the cover id: one image per album.
                            if let Err(_err) = images.store(cover, &bytes) {
                                // The worker must never write to the terminal:
                                // stderr during the alternate screen garbles
                                // the UI. Failures are silent by design.
                            }
                        }
                        // Not every album has art; a miss is normal.
                        Err(_err) => {}
                    }
                }
            }
        }

        let mut miss_keys = Vec::new();
        let mut miss_requests = Vec::new();
        for job in pending {
            let image_key = job.image_key();
            if !images.contains(&image_key) {
                continue;
            }
            miss_keys.push(job.key);
            miss_requests.push(RenderRequest::new(
                images.path_for(&image_key),
                job.cols,
                job.rows,
                job.colors,
            ));
        }

        if miss_requests.is_empty() {
            continue;
        }

        match renderer.render(&miss_requests) {
            Ok(rendered) => {
                for (key, blob) in miss_keys.into_iter().zip(rendered) {
                    let _ = blobs.put(&key, &blob);
                    let dimensions = dimensions_of(renderer.protocol(), &blob);
                    if done_tx
                        .send(Done {
                            key,
                            blob,
                            dimensions,
                            from_cache: false,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
            Err(_err) => {
                // A failed render is not fatal: the UI shows no art for that
                // album and everything else keeps working. Never log to stderr
                // here — the alternate screen owns the terminal.
            }
        }
    }
}

/// Block until there is work, then take one same-shaped batch.
fn take_batch(shared: &Arc<(Mutex<Shared>, Condvar)>) -> Option<Vec<Job>> {
    let (lock, cvar) = &**shared;
    let mut guard = lock.lock().expect("render queue poisoned");

    while guard.queue.is_empty() && !guard.shutdown {
        guard = cvar.wait(guard).expect("render queue poisoned");
    }
    if guard.shutdown || guard.queue.is_empty() {
        return None;
    }

    guard.queue.sort_by_key(|job| job.priority);

    let mut batch: Vec<Job> = Vec::with_capacity(MAX_BATCH);
    let mut shape: Option<(u32, u32, u32)> = None;
    let mut index = 0;
    while index < guard.queue.len() && batch.len() < MAX_BATCH {
        let candidate = guard.queue[index].shape();
        let first = *shape.get_or_insert(candidate);
        if candidate == first {
            batch.push(guard.queue.remove(index));
        } else {
            index += 1;
        }
    }
    Some(batch)
}

/// Drop anything cancelled between dequeue and render.
fn filter_cancelled(shared: &Arc<(Mutex<Shared>, Condvar)>, batch: Vec<Job>) -> Vec<Job> {
    let (lock, _) = &**shared;
    let mut guard = lock.lock().expect("render queue poisoned");
    let mut kept = Vec::with_capacity(batch.len());
    for job in batch {
        if guard.cancelled.remove(&job.key) {
            continue;
        }
        kept.push(job);
    }
    kept
}

fn dimensions_of(protocol: ArtProtocol, blob: &[u8]) -> Option<(u32, u32)> {
    match protocol {
        ArtProtocol::Sixel => sixel_dimensions(blob),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::chafa::RenderError;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    struct FakeRenderer {
        calls: Arc<AtomicUsize>,
        last_batch: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl ArtRenderer for FakeRenderer {
        fn protocol(&self) -> ArtProtocol {
            ArtProtocol::Sixel
        }

        fn render(&self, requests: &[RenderRequest]) -> Result<Vec<Vec<u8>>, RenderError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.last_batch.store(requests.len(), Ordering::SeqCst);
            if !self.delay.is_zero() {
                std::thread::sleep(self.delay);
            }
            Ok(requests
                .iter()
                .map(|r| format!("\x1bPfake-{}", r.cols).into_bytes())
                .collect())
        }
    }

    struct FakeSource {
        calls: Arc<AtomicUsize>,
        fail: bool,
    }

    impl ImageSource for FakeSource {
        fn fetch(&self, _cover_id: &str, _px: u32) -> Result<Vec<u8>, String> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail {
                Err("no art".to_string())
            } else {
                Ok(b"image-bytes".to_vec())
            }
        }
    }

    fn rand_nanos() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    struct Fixture {
        blobs: BlobCache,
        images: ImageCache,
    }

    fn fixture(tag: &str) -> Fixture {
        let base = std::env::temp_dir().join(format!(
            "undertone-worker-{}-{}-{}",
            std::process::id(),
            tag,
            rand_nanos()
        ));
        Fixture {
            blobs: BlobCache::new(base.join("blobs"), 10 * 1024 * 1024).unwrap(),
            images: ImageCache::new(base.join("img")).unwrap(),
        }
    }

    type Harness = (
        RenderWorker,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
        Arc<AtomicUsize>,
    );

    fn spawn(tag: &str, delay: Duration, fail_fetch: bool) -> Harness {
        let calls = Arc::new(AtomicUsize::new(0));
        let batch = Arc::new(AtomicUsize::new(0));
        let fetches = Arc::new(AtomicUsize::new(0));
        let f = fixture(tag);
        let renderer = FakeRenderer {
            calls: Arc::clone(&calls),
            last_batch: Arc::clone(&batch),
            delay,
        };
        let source = FakeSource {
            calls: Arc::clone(&fetches),
            fail: fail_fetch,
        };
        let worker = RenderWorker::new(
            Box::new(renderer),
            f.blobs,
            f.images,
            Some(Arc::new(source)),
        );
        (worker, calls, batch, fetches)
    }

    fn collect(worker: &RenderWorker, expected: usize) -> Vec<Done> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut done = Vec::new();
        while Instant::now() < deadline && done.len() < expected {
            done.extend(worker.drain_done());
            std::thread::sleep(Duration::from_millis(5));
        }
        done
    }

    #[test]
    fn renders_submitted_jobs_and_batches_them() {
        let (mut worker, calls, batch, fetches) = spawn("basic", Duration::ZERO, false);
        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        worker.submit(Job::visible("b", "cover-b", 4, 3, 16));

        let done = collect(&worker, 2);
        assert_eq!(done.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "one batched render");
        assert_eq!(batch.load(Ordering::SeqCst), 2);
        assert_eq!(fetches.load(Ordering::SeqCst), 2, "each cover fetched once");
        worker.shutdown();
    }

    #[test]
    fn a_repeat_request_is_served_from_cache_without_refetching() {
        let (mut worker, calls, _, fetches) = spawn("cachehit", Duration::ZERO, false);
        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        let first = collect(&worker, 1);
        assert!(!first[0].from_cache);

        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        let second = collect(&worker, 1);
        assert!(second[0].from_cache);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no second render");
        assert_eq!(fetches.load(Ordering::SeqCst), 1, "no second fetch");
        worker.shutdown();
    }

    #[test]
    fn different_shapes_do_not_share_a_batch() {
        let (mut worker, calls, _, _) = spawn("shapes", Duration::ZERO, false);
        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        worker.submit(Job::visible("b", "cover-b", 8, 6, 16));
        let done = collect(&worker, 2);
        assert_eq!(done.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        worker.shutdown();
    }

    #[test]
    fn a_missing_cover_is_skipped_not_fatal() {
        let (mut worker, calls, _, fetches) = spawn("nofetch", Duration::ZERO, true);
        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        worker.submit(Job::visible("b", "cover-b", 4, 3, 16));
        let done = collect(&worker, 2);
        assert!(done.is_empty(), "nothing to render without source bytes");
        assert_eq!(fetches.load(Ordering::SeqCst), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        worker.shutdown();
    }

    #[test]
    fn cancelling_drops_queued_work() {
        let (mut worker, calls, _, _) = spawn("cancel", Duration::from_millis(150), false);
        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        std::thread::sleep(Duration::from_millis(40));
        worker.submit(Job::visible("b", "cover-b", 4, 3, 16));
        worker.cancel(&["b".to_string()]);

        let done = collect(&worker, 1);
        std::thread::sleep(Duration::from_millis(250));
        let extra = worker.drain_done();
        let keys: Vec<&str> = done
            .iter()
            .chain(extra.iter())
            .map(|d| d.key.as_str())
            .collect();
        assert!(keys.contains(&"a"));
        assert!(!keys.contains(&"b"), "cancelled job must not render");
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        worker.shutdown();
    }

    #[test]
    fn priority_puts_visible_work_first() {
        let (mut worker, _, _, _) = spawn("priority", Duration::from_millis(80), false);
        worker.submit(Job::visible("busy", "cover-0", 4, 3, 16));
        std::thread::sleep(Duration::from_millis(20));
        worker.submit(Job::prefetch("later", "cover-1", 4, 3, 16));
        worker.submit(Job::visible("now", "cover-2", 4, 3, 16));

        let done = collect(&worker, 3);
        let first_after_busy = done.iter().map(|d| d.key.as_str()).find(|k| *k != "busy");
        assert_eq!(first_after_busy, Some("now"));
        worker.shutdown();
    }

    #[test]
    fn shutdown_is_idempotent_and_stops_the_thread() {
        let (mut worker, _, _, _) = spawn("shutdown", Duration::ZERO, false);
        worker.submit(Job::visible("a", "cover-a", 4, 3, 16));
        worker.shutdown();
        worker.shutdown();
        assert_eq!(worker.pending(), 0);
        worker.submit(Job::visible("b", "cover-b", 4, 3, 16));
        assert_eq!(worker.pending(), 0);
    }
}
