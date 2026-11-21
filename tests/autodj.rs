//! Auto DJ against a deliberately slow local server.
//!
//! The failure this guards against is a frozen screen: the fetch used to run on
//! the UI thread, so a server that took its time stopped the app from handling
//! keys at exactly the moment the queue was running dry.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::time::{Duration, Instant};

use undertone::app::App;
use undertone::art::ArtService;
use undertone::config::{Palette, Settings};
use undertone::player::NullPlayer;
use undertone::queue::QueueItem;
use undertone::render::{ArtProtocol, CellSize};
use undertone::subsonic::{SubsonicClient, SubsonicConfig};

/// How long the server sits on `getSimilarSongs`.
const SERVER_DELAY: Duration = Duration::from_millis(900);
/// Generous enough for the slowest machine, well under the server delay.
const CALLER_LIMIT: Duration = Duration::from_millis(450);

/// Serves both Auto DJ endpoints, slowly.
struct SlowServer {
    base: String,
}

impl SlowServer {
    fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                let target = request.lines().next().unwrap_or_default();
                if target.contains("getSimilarSongs") {
                    std::thread::sleep(SERVER_DELAY);
                }
                // Both payloads in one envelope: the fallback endpoint is only
                // called when the similar answer adds nothing, which is not
                // this test's path, but serving it keeps the fake honest.
                let body = concat!(
                    r#"{"subsonic-response":{"status":"ok","version":"1.16.1","#,
                    r#""similarSongs":{"song":[{"id":"dj-1","title":"One"},"#,
                    r#"{"id":"dj-2","title":"Two"}]},"#,
                    r#""randomSongs":{"song":[{"id":"rnd-1","title":"Random"}]}}}"#
                );
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes());
            }
        });
        Self {
            base: format!("http://{addr}"),
        }
    }
}

fn app_on(server: &SlowServer) -> App {
    let client = SubsonicClient::new(SubsonicConfig {
        server: server.base.clone(),
        username: "demo".into(),
        password: "demo".into(),
    });
    let settings = Settings {
        server: server.base.clone(),
        username: "demo".into(),
        password: "demo".into(),
        scrobble: false,
        auto_dj: true,
        palette: Palette::Partial,
        backdrop: [0, 0, 0],
        cache_dir: std::env::temp_dir().join(format!("undertone-autodj-{}", std::process::id())),
        cache_bytes: 1 << 20,
        mpv_binary: std::path::PathBuf::from("mpv"),
    };
    let art = ArtService::from_settings(ArtProtocol::Symbols, &settings.cache_dir, 1 << 20, None)
        .expect("art");
    App::new(
        client,
        settings,
        Box::new(NullPlayer::default()),
        art,
        ArtProtocol::Symbols,
        CellSize::FALLBACK,
        80,
        undertone::listen_later::ListenLater::load(
            std::env::temp_dir().join(format!("undertone-autodj-ll-{}.json", std::process::id())),
        ),
    )
}

#[test]
fn a_slow_auto_dj_fetch_does_not_block_the_caller() {
    let server = SlowServer::start();
    let mut app = app_on(&server);
    let seed = QueueItem {
        id: "seed".into(),
        title: "seed".into(),
        ..Default::default()
    };
    app.now.track = Some(seed.clone());
    app.queue.replace(vec![seed]);

    let started = Instant::now();
    app.auto_dj_top_up();
    let elapsed = started.elapsed();

    assert!(
        elapsed < CALLER_LIMIT,
        "auto dj blocked the caller for {elapsed:?}"
    );
    assert_eq!(
        app.queue.len(),
        1,
        "the worker must not touch the queue itself"
    );

    // The answer is picked up by the UI thread and spliced in there.
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.queue.len() == 1 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        app.pump();
    }

    assert_eq!(app.queue.len(), 3, "the answer lands through pump");
}
