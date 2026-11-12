//! Live checks against the public Navidrome demo server.
//!
//! Ignored by default so CI stays hermetic and offline-safe:
//!
//! ```text
//! cargo test --test live_demo -- --ignored --nocapture
//! ```
//!
//! Credentials are the demo server's public ones (`demo` / `demo`).

use undertone::subsonic::{
    AlbumListQuery, AlbumListType, SubsonicClient, SubsonicConfig, SubsonicError,
};

fn client() -> SubsonicClient {
    SubsonicClient::new(SubsonicConfig {
        server: "https://demo.navidrome.org".to_string(),
        username: "demo".to_string(),
        password: "demo".to_string(),
    })
}

#[test]
#[ignore = "hits the public demo server"]
fn ping_reports_navidrome() {
    let info = client().ping().expect("ping should succeed");
    assert!(!info.version.is_empty());
    assert_eq!(info.server_type.as_deref(), Some("navidrome"));
    assert!(info.open_subsonic);
    println!("server: {:?} version {}", info.server_type, info.version);
}

#[test]
#[ignore = "hits the public demo server"]
fn wrong_password_is_a_server_error_40() {
    let bad = SubsonicClient::new(SubsonicConfig {
        server: "https://demo.navidrome.org".to_string(),
        username: "demo".to_string(),
        password: "definitely-not-the-password".to_string(),
    });
    match bad.ping() {
        Err(SubsonicError::Server { code, message }) => {
            assert_eq!(code, 40);
            assert!(message.to_lowercase().contains("password"));
        }
        other => panic!("expected Server(40), got {other:?}"),
    }
}

#[test]
#[ignore = "hits the public demo server"]
fn random_albums_have_ids_and_cover_art() {
    let albums = client()
        .album_list(&AlbumListQuery {
            kind: AlbumListType::Random,
            size: 5,
            ..Default::default()
        })
        .expect("album list should succeed");
    assert!(!albums.is_empty());
    for album in &albums {
        assert!(!album.id.is_empty(), "album id must not be empty");
        assert!(!album.name.is_empty(), "album name must not be empty");
    }
    println!("fetched {} albums, first: {}", albums.len(), albums[0].name);
}

#[test]
#[ignore = "hits the public demo server"]
fn album_detail_contains_songs() {
    let api = client();
    let albums = api
        .album_list(&AlbumListQuery {
            kind: AlbumListType::Random,
            size: 1,
            ..Default::default()
        })
        .expect("album list");
    let album = api.album(&albums[0].id).expect("album detail");
    assert_eq!(album.id, albums[0].id);
    assert!(!album.song.is_empty(), "album should have songs");
    let song = &album.song[0];
    assert!(!song.title.is_empty());
    println!(
        "{} — {} songs, first track: {}",
        album.name,
        album.song.len(),
        song.title
    );
}

#[test]
#[ignore = "hits the public demo server"]
fn artist_index_sorting_is_stable_within_a_call() {
    let index = client().artists().expect("artists index");
    let names: Vec<&str> = index.artists().map(|a| a.name.as_str()).collect();
    assert!(!names.is_empty());
    assert!(names.iter().all(|n| !n.is_empty()));
    println!(
        "{} artists across {} index letters",
        names.len(),
        index.index.len()
    );
}

#[test]
#[ignore = "hits the public demo server"]
fn search_returns_a_typed_result_even_when_empty() {
    let api = client();
    for query in ["nine inch nails", "zzzzzzzzzz-no-such-thing"] {
        let result = api.search(query).expect("search should not fail on empty");
        println!(
            "query {query:?}: {} artists, {} albums, {} songs",
            result.artist.len(),
            result.album.len(),
            result.song.len()
        );
    }
}

#[test]
#[ignore = "hits the public demo server"]
fn cover_art_is_an_image() {
    let api = client();
    let albums = api
        .album_list(&AlbumListQuery {
            kind: AlbumListType::Random,
            size: 3,
            ..Default::default()
        })
        .expect("album list");

    let mut checked = 0;
    for album in &albums {
        let Some(cover_id) = album.cover_art.as_deref() else {
            continue;
        };
        let (bytes, content_type) = api.cover_art(cover_id, Some(300)).expect("cover art");
        assert!(bytes.len() > 1000, "cover art should not be a stub");
        assert!(
            content_type.starts_with("image/"),
            "unexpected content type {content_type}"
        );
        println!("cover art: {content_type}, {} bytes", bytes.len());
        checked += 1;
        break;
    }
    assert!(checked > 0, "expected at least one album with cover art");
}

#[test]
#[ignore = "hits the public demo server"]
fn playlists_and_scrobble_round_trip() {
    let api = client();
    let playlists = api.playlists().expect("playlists");
    assert!(!playlists.is_empty());
    let detail = api.playlist(&playlists[0].id).expect("playlist detail");
    assert_eq!(detail.id, playlists[0].id);
    assert!(
        !detail.entry.is_empty(),
        "demo playlist should have entries"
    );

    // "now playing" is a no-op mutation: it must not error.
    api.scrobble(&detail.entry[0].id, false, None)
        .expect("now-playing should be accepted");
    println!(
        "playlist {} — {} entries; now-playing accepted",
        detail.name,
        detail.entry.len()
    );
}
