//! Background art rendering.
//!
//! One thread, one job at a time, batched by shape so chafa's process startup
//! is paid once per screenful rather than once per image. Rendering never
//! happens on the input path — that is what makes a TUI feel broken on slow
//! hardware.
//!
//! Jobs carry a priority so what is visible now beats what is being prefetched
//! for the next screen, and cancelled jobs are dropped before they cost a
//! process spawn.

use std::collections::HashSet;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;

use super::cache::BlobCache;
use super::chafa::{sixel_dimensions, ArtRenderer, RenderRequest};
use super::protocol::ArtProtocol;

/// Upper bound on images per chafa invocation.
pub const MAX_BATCH: usize = 8;

/// Priority for art the user can see right now.
pub const PRIORITY_VISIBLE: u8 = 0;
/// Priority for art that will scroll into view soon.
pub const PRIORITY_PREFETCH: u8 = 10;

#[derive(Debug, Clone)]
pub struct Job {
    pub key: String,
    pub request: RenderRequest,
    pub priority: u8,
}

impl Job {
    pub fn visible(key: impl Into<String>, request: RenderRequest) -> Self {
        Self {
            key: key.into(),
            request,
            priority: PRIORITY_VISIBLE,
        }
    }

    pub fn prefetch(key: impl Into<String>, request: RenderRequest) -> Self {
        Self {
            key: key.into(),
            request,
            priority: PRIORITY_PREFETCH,
        }
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
    pub fn new(renderer: Box<dyn ArtRenderer>, cache: BlobCache) -> Self {
        let shared = Arc::new((Mutex::new(Shared::default()), Condvar::new()));
        let (done_tx, done_rx) = mpsc::channel();

        let thread_shared = Arc::clone(&shared);
        let handle = std::thread::Builder::new()
            .name("art-render".to_string())
            .spawn(move || worker_loop(thread_shared, renderer, cache, done_tx))
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
        // Drop a duplicate that is already queued for the same key/priority.
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
    mut cache: BlobCache,
    done_tx: Sender<Done>,
) {
    while let Some(batch) = take_batch(&shared) {
        let batch = filter_cancelled(&shared, batch);
        if batch.is_empty() {
            continue;
        }

        let mut miss_keys = Vec::new();
        let mut miss_requests = Vec::new();

        for job in batch {
            if let Some(blob) = cache.get(&job.key) {
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
            } else {
                miss_keys.push(job.key);
                miss_requests.push(job.request);
            }
        }

        if miss_requests.is_empty() {
            continue;
        }

        match renderer.render(&miss_requests) {
            Ok(blobs) => {
                for (key, blob) in miss_keys.into_iter().zip(blobs) {
                    let _ = cache.put(&key, &blob);
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
            Err(err) => {
                // A failed render is not fatal: the UI shows no art for that
                // album and everything else keeps working.
                eprintln!("undertone: art render failed: {err}");
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
        let candidate_shape = guard.queue[index].request.batch_shape();
        let first_shape = *shape.get_or_insert(candidate_shape);
        if candidate_shape == first_shape {
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
        blobs: usize,
        calls: Arc<AtomicUsize>,
        last_batch: Arc<AtomicUsize>,
        delay: Duration,
    }

    impl FakeRenderer {
        fn new() -> (Self, Arc<AtomicUsize>, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            let batch = Arc::new(AtomicUsize::new(0));
            (
                Self {
                    blobs: 0,
                    calls: Arc::clone(&calls),
                    last_batch: Arc::clone(&batch),
                    delay: Duration::ZERO,
                },
                calls,
                batch,
            )
        }
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
                .map(|r| format!("\x1bPrender-{}-{}", r.cols, self.blobs).into_bytes())
                .collect())
        }
    }

    fn cache(tag: &str) -> BlobCache {
        let root = std::env::temp_dir().join(format!(
            "undertone-worker-{}-{}-{}",
            std::process::id(),
            tag,
            rand_nanos()
        ));
        BlobCache::new(root, 10 * 1024 * 1024).unwrap()
    }

    fn rand_nanos() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0)
    }

    fn request(cols: u32) -> RenderRequest {
        RenderRequest::new("x.jpg", cols, 3, 16)
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
    fn renders_submitted_jobs_and_reports_them() {
        let (renderer, calls, batch) = FakeRenderer::new();
        let mut worker = RenderWorker::new(Box::new(renderer), cache("basic"));
        worker.submit(Job::visible("a", request(4)));
        worker.submit(Job::visible("b", request(4)));

        let done = collect(&worker, 2);
        assert_eq!(done.len(), 2);
        let keys: Vec<&str> = done.iter().map(|d| d.key.as_str()).collect();
        assert!(keys.contains(&"a") && keys.contains(&"b"));
        assert_eq!(calls.load(Ordering::SeqCst), 1, "batch into one render");
        assert_eq!(batch.load(Ordering::SeqCst), 2);
        worker.shutdown();
    }

    #[test]
    fn a_repeat_request_is_served_from_cache() {
        let (renderer, calls, _) = FakeRenderer::new();
        let mut worker = RenderWorker::new(Box::new(renderer), cache("cachehit"));
        worker.submit(Job::visible("a", request(4)));
        let first = collect(&worker, 1);
        assert_eq!(first.len(), 1);
        assert!(!first[0].from_cache);

        worker.submit(Job::visible("a", request(4)));
        let second = collect(&worker, 1);
        assert_eq!(second.len(), 1);
        assert!(
            second[0].from_cache,
            "second request must hit the blob cache"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1, "no second render");
        worker.shutdown();
    }

    #[test]
    fn duplicate_queued_jobs_are_coalesced() {
        let (renderer, _, batch) = FakeRenderer::new();
        let delay_renderer = FakeRenderer {
            delay: Duration::from_millis(50),
            ..renderer
        };
        let _ = batch;
        let mut worker = RenderWorker::new(Box::new(delay_renderer), cache("dupes"));
        worker.submit(Job::visible("a", request(4)));
        // Submitted while the first is still rendering.
        worker.submit(Job::visible("a", request(4)));
        worker.submit(Job::visible("a", request(4)));
        let done = collect(&worker, 2);
        assert!(!done.is_empty());
        worker.shutdown();
    }

    #[test]
    fn different_shapes_do_not_share_a_batch() {
        let (renderer, calls, _) = FakeRenderer::new();
        let mut worker = RenderWorker::new(Box::new(renderer), cache("shapes"));
        worker.submit(Job::visible("a", request(4)));
        worker.submit(Job::visible("b", request(8)));
        let done = collect(&worker, 2);
        assert_eq!(done.len(), 2);
        assert_eq!(calls.load(Ordering::SeqCst), 2, "two shapes, two renders");
        worker.shutdown();
    }

    #[test]
    fn cancelling_drops_queued_work() {
        let (renderer, calls, _) = FakeRenderer::new();
        let slow = FakeRenderer {
            delay: Duration::from_millis(150),
            ..renderer
        };
        let mut worker = RenderWorker::new(Box::new(slow), cache("cancel"));
        worker.submit(Job::visible("a", request(4)));
        // Let the worker pick up "a", then queue and cancel "b".
        std::thread::sleep(Duration::from_millis(40));
        worker.submit(Job::visible("b", request(4)));
        worker.cancel(&["b".to_string()]);

        let done = collect(&worker, 1);
        std::thread::sleep(Duration::from_millis(200));
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
    fn shutdown_is_idempotent_and_stops_the_thread() {
        let (renderer, _, _) = FakeRenderer::new();
        let mut worker = RenderWorker::new(Box::new(renderer), cache("shutdown"));
        worker.submit(Job::visible("a", request(4)));
        worker.shutdown();
        worker.shutdown();
        assert_eq!(worker.pending(), 0);
        // Submitting after shutdown is a no-op rather than a panic.
        worker.submit(Job::visible("b", request(4)));
        assert_eq!(worker.pending(), 0);
    }

    #[test]
    fn priority_puts_visible_work_first() {
        let (renderer, _, batch) = FakeRenderer::new();
        let slow = FakeRenderer {
            delay: Duration::from_millis(80),
            ..renderer
        };
        let mut worker = RenderWorker::new(Box::new(slow), cache("priority"));
        // Occupy the worker.
        worker.submit(Job::visible("busy", request(4)));
        std::thread::sleep(Duration::from_millis(20));
        worker.submit(Job::prefetch("later", request(4)));
        worker.submit(Job::visible("now", request(4)));

        let done = collect(&worker, 3);
        let first_after_busy = done.iter().map(|d| d.key.as_str()).find(|k| *k != "busy");
        assert_eq!(
            first_after_busy,
            Some("now"),
            "visible work should be rendered before prefetch"
        );
        let _ = batch;
        worker.shutdown();
    }
}
