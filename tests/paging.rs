//! Album paging against a local server, so it runs offline and repeatably.
//!
//! The demo Navidrome only holds fifty albums, which is fewer than one page —
//! a short page is exactly the case that *stops* paging, so the interesting
//! path (append, then stop at the end) cannot be exercised there.

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use undertone::app::App;
use undertone::art::ArtService;
use undertone::config::Settings;
use undertone::player::NullPlayer;
use undertone::render::ArtProtocol;
use undertone::subsonic::{SubsonicClient, SubsonicConfig};

/// Serves `getAlbumList2` from a fixed catalogue, honouring size and offset.
struct FakeServer {
    base: String,
    requests: Arc<AtomicUsize>,
    total: usize,
}

impl FakeServer {
    fn start(total: usize) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let requests = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&requests);

        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let mut buf = [0u8; 4096];
                let read = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..read]).to_string();
                counter.fetch_add(1, Ordering::SeqCst);

                let query = request
                    .lines()
                    .next()
                    .and_then(|line| line.split_whitespace().nth(1))
                    .and_then(|target| target.split_once('?').map(|(_, q)| q.to_string()))
                    .unwrap_or_default();
                let param = |name: &str| -> usize {
                    query
                        .split('&')
                        .filter_map(|pair| pair.split_once('='))
                        .find(|(k, _)| *k == name)
                        .and_then(|(_, v)| v.parse().ok())
                        .unwrap_or(0)
                };
                let size = param("size").clamp(1, 500);
                let offset = param("offset");
                let end = (offset + size).min(total);
                let albums: Vec<String> = (offset..end)
                    .map(|i| format!(r#"{{"id":"al-{i}","name":"Album {i}","coverArt":"al-{i}"}}"#))
                    .collect();
                let body = format!(
                    r#"{{"subsonic-response":{{"status":"ok","version":"1.16.1","albumList2":{{"album":[{}]}}}}}}"#,
                    albums.join(",")
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
            requests,
            total,
        }
    }
}

fn app_on(server: &FakeServer) -> App {
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
        auto_dj: false,
        art_mode: "off".into(),
        cover_colors: 16,
        thumb_colors: 16,
        transcode: false,
        cache_dir: std::path::PathBuf::from("/tmp/undertone-paging"),
        cache_bytes: 1 << 20,
        mpv_binary: std::path::PathBuf::from("mpv"),
    };
    let dir = std::env::temp_dir().join(format!(
        "undertone-paging-{}-{}",
        std::process::id(),
        server.requests.load(Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    let art = ArtService::from_settings(ArtProtocol::Symbols, &dir, 1 << 20, None).expect("art");
    App::new(
        client,
        settings,
        Box::new(NullPlayer::default()),
        art,
        ArtProtocol::Symbols,
        80,
    )
}

#[test]
fn scrolling_past_a_page_appends_and_then_stops() {
    let server = FakeServer::start(250);
    let mut app = app_on(&server);

    app.load_albums();
    assert_eq!(app.albums.len(), App::ALBUM_PAGE, "first page");
    assert!(
        !app.albums_exhausted,
        "250 albums means there is more to come"
    );

    // Walk to the end of what is loaded; the lookahead should pull page two.
    app.selected = app.albums.len() - 1;
    app.maybe_load_more();
    assert_eq!(
        app.albums.len(),
        2 * App::ALBUM_PAGE,
        "second page appended"
    );

    app.selected = app.albums.len() - 1;
    app.maybe_load_more();
    assert_eq!(app.albums.len(), 250, "last page is short");
    assert!(app.albums_exhausted, "a short page ends the list");

    // No further requests once exhausted, however far the cursor goes.
    let before = server.requests.load(Ordering::SeqCst);
    app.selected = app.albums.len() - 1;
    app.maybe_load_more();
    app.maybe_load_more();
    assert_eq!(server.requests.load(Ordering::SeqCst), before);
    assert_eq!(app.albums.len(), 250);
    assert_eq!(server.total, 250);
}

#[test]
fn paging_preserves_order_without_duplicates() {
    let server = FakeServer::start(250);
    let mut app = app_on(&server);
    app.load_albums();
    while !app.albums_exhausted {
        app.selected = app.albums.len() - 1;
        app.maybe_load_more();
    }

    let ids: Vec<&str> = app.albums.iter().map(|a| a.id.as_str()).collect();
    assert_eq!(ids.len(), 250);
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), ids.len(), "a page was fetched twice");
    assert_eq!(ids.first(), Some(&"al-0"));
    assert_eq!(ids.last(), Some(&"al-249"));
}

#[test]
fn a_catalogue_smaller_than_one_page_never_pages() {
    let server = FakeServer::start(12);
    let mut app = app_on(&server);
    app.load_albums();

    assert_eq!(app.albums.len(), 12);
    assert!(app.albums_exhausted);
    let before = server.requests.load(Ordering::SeqCst);
    app.selected = 11;
    app.maybe_load_more();
    assert_eq!(
        server.requests.load(Ordering::SeqCst),
        before,
        "no stray request"
    );
}
