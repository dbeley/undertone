//! Measures the art pipeline end to end: cold render, warm cache, and the
//! cost of an unchanged frame.
//!
//! ```text
//! cargo run --release --example art_bench
//! ```

use std::time::{Duration, Instant};

use undertone::render::{
    ArtProtocol, BlobCache, ChafaRenderer, Job, Painter, Placement, RenderRequest, RenderWorker,
};

const THUMBNAILS: usize = 12;
const IMAGE: &str = "tests/fixtures/cover-test.jpg";

fn main() {
    let root = std::env::temp_dir().join(format!("undertone-bench-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let cache = BlobCache::new(&root, 20 * 1024 * 1024).expect("cache");

    let protocol = ArtProtocol::Sixel;
    println!("protocol: {}", protocol.label());
    println!("thumbnails per screen: {THUMBNAILS}");

    let renderer = ChafaRenderer::with_default_binary(protocol);
    let mut worker = RenderWorker::new(Box::new(renderer), cache);

    let cold = measure(&mut worker, "cold", Duration::from_secs(60));
    let warm = measure(&mut worker, "warm", Duration::from_secs(10));

    println!("\n--- painter ---");
    let mut painter = Painter::new(protocol);
    let placements: Vec<Placement> = (0..THUMBNAILS)
        .map(|i| Placement::new(format!("album{i}"), (i as u16) * 2, 0, 2, 4))
        .collect();

    let first = painter.plan(&placements);
    let idle = painter.plan(&placements);
    println!("first frame:  {} ops", first.op_count());
    println!("idle frame:   {} ops (must be 0)", idle.op_count());

    let start = Instant::now();
    for _ in 0..1000 {
        std::hint::black_box(painter.plan(&placements));
    }
    println!(
        "plan() idle:  {:.1} us/call",
        start.elapsed().as_secs_f64() * 1_000_000.0 / 1000.0
    );

    println!("\ncold: {:?}   warm: {:?}", cold.duration, warm.duration);
    if cold.blobs > 0 {
        println!(
            "avg blob: {:.0} bytes/thumbnail",
            cold.bytes as f64 / cold.blobs as f64
        );
    }

    worker.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

struct Sample {
    duration: Duration,
    blobs: usize,
    bytes: usize,
}

fn measure(worker: &mut RenderWorker, label: &str, timeout: Duration) -> Sample {
    let start = Instant::now();
    for i in 0..THUMBNAILS {
        worker.submit(Job::visible(
            format!("album{i}"),
            RenderRequest::new(IMAGE, 4, 3, 16),
        ));
    }

    let mut blobs = 0;
    let mut bytes = 0;
    let mut cached = 0;
    while blobs < THUMBNAILS && start.elapsed() < timeout {
        for done in worker.drain_done() {
            blobs += 1;
            bytes += done.blob.len();
            if done.from_cache {
                cached += 1;
            }
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    let duration = start.elapsed();
    println!(
        "{label:<5} {THUMBNAILS} thumbnails in {:>8.1?}  ({blobs} blobs, {cached} from cache, {} bytes)",
        duration, bytes
    );
    Sample {
        duration,
        blobs,
        bytes,
    }
}
