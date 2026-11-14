//! Error paths, without a real server or a real mpv.
//!
//! Every failure here is one a user can actually hit — a wrong password, a
//! proxy returning an HTML error page, a server that is down, a missing player,
//! a cache directory that cannot be written. The rule the design sets is that
//! none of them may be fatal: they surface in the status line and the app keeps
//! running.

use std::io::{Read, Write};
use std::net::TcpListener;

use undertone::app::App;
use undertone::art::ArtService;
use undertone::config::Settings;
use undertone::player::{MpvOptions, MpvPlayer, NullPlayer};
use undertone::render::{ArtProtocol, CellSize};
use undertone::subsonic::{SubsonicClient, SubsonicConfig, SubsonicError};

/// A server that answers every request with the same fixed body.
fn serve_once(body: &'static str, status_line: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut buf = [0u8; 2048];
            let _ = stream.read(&mut buf);
            let response = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

fn client_for(server: &str) -> SubsonicClient {
    SubsonicClient::new(SubsonicConfig {
        server: server.to_string(),
        username: "demo".into(),
        password: "demo".into(),
    })
}

fn settings_for(server: &str) -> Settings {
    Settings {
        server: server.to_string(),
        username: "demo".into(),
        password: "demo".into(),
        scrobble: false,
        auto_dj: false,
        art_mode: "off".into(),
        thumb_colors: 16,
        cover_colors: 16,
        cache_dir: std::env::temp_dir().join("undertone-errors-cache"),
        cache_bytes: 1 << 20,
        mpv_binary: std::path::PathBuf::from("mpv"),
        transcode: false,
        backdrop: [0, 0, 0],
    }
}

fn app_for(server: &str) -> App {
    let dir = std::env::temp_dir().join(format!(
        "undertone-errors-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let art = ArtService::from_settings(ArtProtocol::Off, &dir, 1 << 20, None).expect("art");
    App::new(
        client_for(server),
        settings_for(server),
        Box::new(NullPlayer::new()),
        art,
        ArtProtocol::Off,
        CellSize::FALLBACK,
        70,
        undertone::listen_later::ListenLater::load(dir.join("ll.json")),
    )
}

#[test]
fn a_wrong_password_is_a_server_error_with_the_code_intact() {
    let body = r#"{"subsonic-response":{"status":"failed","version":"1.16.1","error":{"code":40,"message":"Wrong username or password"}}}"#;
    let server = serve_once(body, "HTTP/1.1 200 OK");
    match client_for(&server).ping() {
        Err(SubsonicError::Server { code, message }) => {
            assert_eq!(code, 40);
            assert!(message.contains("Wrong username"), "{message}");
        }
        other => panic!("expected a server error, got {other:?}"),
    }
}

#[test]
fn an_html_error_page_is_a_decode_error_not_a_panic() {
    let server = serve_once("<html>502 bad gateway</html>", "HTTP/1.1 200 OK");
    match client_for(&server).ping() {
        Err(SubsonicError::Decode(_)) => {}
        other => panic!("expected a decode error, got {other:?}"),
    }
}

#[test]
fn a_non_2xx_answer_reports_the_status_and_endpoint() {
    let server = serve_once("{}", "HTTP/1.1 500 Internal Server Error");
    match client_for(&server).ping() {
        Err(SubsonicError::Http { status, endpoint }) => {
            assert_eq!(status, 500);
            assert!(endpoint.contains("ping"), "{endpoint}");
        }
        other => panic!("expected an http error, got {other:?}"),
    }
}

#[test]
fn an_unreachable_server_is_a_transport_error() {
    // Bind and drop, so the port is almost certainly closed.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    match client_for(&format!("http://{addr}")).ping() {
        Err(SubsonicError::Transport(_)) => {}
        other => panic!("expected a transport error, got {other:?}"),
    }
}

#[test]
fn a_failed_album_load_does_not_kill_the_app() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);

    let mut app = app_for(&format!("http://{addr}"));
    app.load_albums();

    // Reported, not fatal, and the app is still usable.
    let status = app.status.as_ref().expect("a status message");
    assert!(status.error, "a failed load must be shown as an error");
    assert!(app.albums.is_empty());
    assert!(!app.should_quit, "a network failure must not quit the app");
}

#[test]
fn a_missing_mpv_is_an_error_rather_than_a_panic() {
    let options = MpvOptions {
        binary: std::path::PathBuf::from("/nonexistent/mpv-for-undertone-test"),
        ..Default::default()
    };
    match MpvPlayer::spawn(&options) {
        Err(undertone::player::PlayerError::Spawn(message)) => {
            assert!(message.contains("mpv-for-undertone-test"), "{message}");
        }
        Err(other) => panic!("expected a spawn error, got {other}"),
        Ok(_) => panic!("expected a spawn error, but mpv started"),
    }
}

#[test]
fn an_unwritable_cache_directory_is_an_error_the_caller_can_survive() {
    // A regular file where a directory is expected: create_dir_all fails.
    let path = std::env::temp_dir().join(format!("undertone-not-a-dir-{}", std::process::id()));
    std::fs::write(&path, b"not a directory").unwrap();

    let result = ArtService::from_settings(ArtProtocol::Sixel, &path, 1 << 20, None);
    assert!(result.is_err(), "the failure must be reported, not ignored");

    let _ = std::fs::remove_file(&path);
}
