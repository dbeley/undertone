//! Resource guards.
//!
//! The whole premise of this project is that it runs on hardware where a
//! hundred megabytes and a busy redraw loop are not affordable. These tests
//! pin the parts that can be checked deterministically in CI: the idle frame
//! must cost nothing, planning must stay in the microsecond range, and building
//! the app must not wander off into I/O.
//!
//! Process-level numbers — RSS and time-to-first-frame — are measured by
//! `scripts/measure.sh`, because they need a real terminal and a real binary,
//! and a threshold on RSS in CI measures the CI runner, not the app.
//!
//! Thresholds are deliberately loose: this is a regression alarm, not a
//! benchmark. A debug build on a loaded runner is several times slower than the
//! release build on the target machine.

use std::time::{Duration, Instant};

use undertone::app::App;
use undertone::art::ArtService;
use undertone::config::Settings;
use undertone::listen_later::ListenLater;
use undertone::player::NullPlayer;
use undertone::queue::QueueItem;
use undertone::render::{ArtProtocol, FramePlan, Painter, Placement};
use undertone::subsonic::{SubsonicClient, SubsonicConfig};

fn settings(cache: std::path::PathBuf) -> Settings {
    Settings {
        server: "http://127.0.0.1:1".into(),
        username: "demo".into(),
        password: "demo".into(),
        scrobble: false,
        auto_dj: false,
        art_mode: "sixel".into(),
        thumb_colors: 16,
        cover_colors: 240,
        cache_dir: cache,
        cache_bytes: 20 * 1024 * 1024,
        mpv_binary: std::path::PathBuf::from("mpv"),
        transcode: false,
        backdrop: [0, 0, 0],
    }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("undertone-perf-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn screen_of(n: u16) -> Vec<Placement> {
    (0..n)
        .map(|i| Placement::new(format!("al-{i}/4x2-16-sixel"), i * 2, 0, 2, 4))
        .collect()
}

#[test]
fn an_idle_frame_plans_to_nothing() {
    let mut painter = Painter::new(ArtProtocol::Sixel);
    let placements = screen_of(14);
    painter.plan(&placements);

    let plan: FramePlan = painter.plan(&placements);
    assert!(plan.is_empty(), "a still screen must emit zero bytes");
    assert_eq!(plan.op_count(), 0);
}

#[test]
fn planning_a_still_screen_stays_in_the_microsecond_range() {
    let mut painter = Painter::new(ArtProtocol::Sixel);
    let placements = screen_of(30);
    painter.plan(&placements);

    let iterations = 5_000;
    let start = Instant::now();
    for _ in 0..iterations {
        let plan = painter.plan(&placements);
        std::hint::black_box(plan);
    }
    let per_call = start.elapsed() / iterations;

    println!("idle plan: {per_call:?} per frame");
    // Measured at ~1.4 µs on an N100 release build; 200 µs in a debug build on
    // a shared runner would still be caught here.
    assert!(
        per_call < Duration::from_micros(200),
        "idle planning regressed to {per_call:?} per frame"
    );
}

#[test]
fn a_full_repaint_plan_is_linear_and_cheap() {
    let mut painter = Painter::new(ArtProtocol::Sixel);
    let placements = screen_of(30);

    let start = Instant::now();
    let plan = painter.plan(&placements);
    let elapsed = start.elapsed();

    assert_eq!(plan.paints.len(), 30, "every new placement is painted once");
    println!("cold plan of 30 placements: {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(50),
        "cold plan took {elapsed:?}"
    );
}

#[test]
fn building_the_app_does_not_touch_the_network() {
    // Startup must be local: no ping, no album list, no art fetch. The server
    // here does not exist, so any I/O would hang or fail loudly.
    let dir = temp_dir("startup");
    let start = Instant::now();
    let art = ArtService::from_settings(ArtProtocol::Sixel, &dir, 1 << 20, None).expect("art");
    let app = App::new(
        SubsonicClient::new(SubsonicConfig {
            server: "http://127.0.0.1:1".into(),
            username: "demo".into(),
            password: "demo".into(),
        }),
        settings(dir.clone()),
        Box::new(NullPlayer::new()),
        art,
        ArtProtocol::Sixel,
        70,
        ListenLater::load(dir.join("ll.json")),
    );
    let elapsed = start.elapsed();

    println!("app construction: {elapsed:?}");
    assert!(app.albums.is_empty());
    assert!(
        elapsed < Duration::from_millis(500),
        "construction took {elapsed:?}; something is doing I/O at startup"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_blob_cache_is_fast_enough_for_scrolling() {
    use undertone::render::BlobCache;
    let dir = temp_dir("cache");
    let mut cache = BlobCache::new(&dir, 4 * 1024 * 1024).expect("cache");

    // A realistic blob: escape-prefixed and terminated, so the cache's
    // integrity check accepts it.
    let mut payload = b"\x1bP".to_vec();
    payload.extend_from_slice(&vec![7u8; 896]);
    payload.extend_from_slice(b"\x1b\\");
    let start = Instant::now();
    for i in 0..100 {
        cache
            .put(&format!("al-{i}/4x2-16-sixel"), &payload)
            .unwrap();
    }
    let write = start.elapsed();

    let start = Instant::now();
    for i in 0..100 {
        assert!(cache.get(&format!("al-{i}/4x2-16-sixel")).is_some());
    }
    let read = start.elapsed();

    println!("cache: 100 writes {write:?}, 100 reads {read:?}");
    assert!(write < Duration::from_secs(2), "writes took {write:?}");
    assert!(read < Duration::from_secs(1), "reads took {read:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn queue_navigation_is_cheap() {
    let mut queue = undertone::queue::Queue::with_seed(1);
    queue.replace(
        (0..500)
            .map(|i| QueueItem {
                id: format!("s{i}"),
                title: format!("track {i}"),
                ..Default::default()
            })
            .collect(),
    );

    let start = Instant::now();
    for _ in 0..10_000 {
        queue.advance();
    }
    let elapsed = start.elapsed();

    println!("10k advances: {elapsed:?}");
    assert!(
        elapsed < Duration::from_secs(1),
        "advance is O(n) somewhere: {elapsed:?}"
    );
}
