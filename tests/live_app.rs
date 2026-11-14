//! End-to-end checks that drive the real `App` against the public demo server.
//!
//! Ignored by default so CI stays hermetic:
//!
//! ```text
//! cargo test --test live_app -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use undertone::app::{App, View};
use undertone::art::ArtService;
use undertone::config::Settings;
use undertone::player::NullPlayer;
use undertone::render::{ArtProtocol, CellSize};
use undertone::subsonic::{SubsonicClient, SubsonicConfig};

fn app() -> App {
    let client = SubsonicClient::new(SubsonicConfig {
        server: "https://demo.navidrome.org".to_string(),
        username: "demo".to_string(),
        password: "demo".to_string(),
    });
    let settings = Settings {
        server: "https://demo.navidrome.org".to_string(),
        username: "demo".to_string(),
        password: "demo".to_string(),
        scrobble: false,
        auto_dj: false,
        art_mode: "off".to_string(),
        thumb_colors: 16,
        cover_colors: 240,
        cache_dir: PathBuf::from("/tmp/undertone-live-test"),
        cache_bytes: 1024 * 1024,
        mpv_binary: PathBuf::from("mpv"),
        transcode: false,
        backdrop: [0, 0, 0],
    };
    let art = ArtService::from_settings(
        ArtProtocol::Off,
        &settings.cache_dir,
        settings.cache_bytes,
        None,
    )
    .expect("art service");
    App::new(
        client,
        settings,
        Box::new(NullPlayer::new()),
        art,
        ArtProtocol::Off,
        CellSize::FALLBACK,
        70,
        undertone::listen_later::ListenLater::load(
            std::env::temp_dir().join("undertone-live-ll.json"),
        ),
    )
}

#[test]
#[ignore = "hits the public demo server"]
fn the_app_loads_a_real_album_list() {
    let mut app = app();
    app.load_albums();
    assert!(
        !app.albums.is_empty(),
        "expected albums from the demo server"
    );
    assert!(
        app.albums.iter().all(|album| !album.id.is_empty()),
        "every album needs an id"
    );
    println!(
        "loaded {} albums, first: {}",
        app.albums.len(),
        app.albums[0].name
    );
}

#[test]
#[ignore = "hits the public demo server"]
fn opening_an_album_and_playing_it_from_the_ui_path() {
    let mut app = app();
    app.load_albums();
    app.open_selected_album();

    assert_eq!(app.view, View::AlbumDetail, "Enter should open the album");
    let (name, song_count, first_title) = {
        let detail = app.detail.as_ref().expect("album detail");
        assert!(!detail.song.is_empty(), "album should have tracks");
        (
            detail.name.clone(),
            detail.song.len(),
            detail.song[0].title.clone(),
        )
    };
    println!("opened {name} — {song_count} tracks");

    app.play_album();
    assert_eq!(app.queue.len(), song_count);
    assert_eq!(
        app.now.track.as_ref().map(|t| t.title.as_str()),
        Some(first_title.as_str())
    );
    println!("queue now: {}", app.queue.len());
}

#[test]
#[ignore = "hits the public demo server"]
fn changing_sort_refetches_and_resets_selection() {
    let mut app = app();
    app.load_albums();
    app.selected = 3;
    app.cycle_sort();
    assert_eq!(app.selected, 0);
    assert!(!app.albums.is_empty());
    println!("sort is now {}", app.sort.as_str());
}

#[test]
#[ignore = "hits the public demo server"]
fn skipping_tracks_walks_the_queue() {
    let mut app = app();
    app.load_albums();
    app.open_selected_album();
    app.play_album();
    let first = app.now.track.as_ref().map(|t| t.title.clone());
    app.next_track();
    let second = app.now.track.as_ref().map(|t| t.title.clone());
    assert_ne!(first, second, "next should move to another track");
    println!("{first:?} -> {second:?}");
}

#[test]
#[ignore]
fn artists_can_be_sorted_three_ways_against_a_real_server() {
    use undertone::app::ArtistSort;

    let mut app = app();
    app.open_artists();
    assert!(!app.artists.is_empty(), "the demo server has artists");

    // Default: most albums first.
    let counts: Vec<u32> = app
        .artists
        .iter()
        .map(|artist| artist.album_count.unwrap_or(0))
        .collect();
    assert!(
        counts.windows(2).all(|pair| pair[0] >= pair[1]),
        "most-albums order is descending: {counts:?}"
    );

    // A-Z.
    app.artist_sort = ArtistSort::Name;
    app.sort_artists();
    let names: Vec<String> = app.artists.iter().map(|a| a.name.to_lowercase()).collect();
    assert!(
        names.windows(2).all(|pair| pair[0] <= pair[1]),
        "name order is ascending"
    );

    // Most played: fills the rank map from the frequent album list, and the
    // order actually changes.
    let before: Vec<String> = app.artists.iter().map(|a| a.id.clone()).collect();
    app.artist_sort = ArtistSort::MostPlayed;
    app.sort_artists();
    assert!(
        !app.artist_play_rank.is_empty(),
        "the frequent list should have ranked some artists"
    );
    let after: Vec<String> = app.artists.iter().map(|a| a.id.clone()).collect();
    assert_ne!(after, before, "most-played ordering differs from a-z");

    // Ranked artists come before unranked ones.
    let mut seen_unranked = false;
    for artist in &app.artists {
        let ranked = app.artist_play_rank.contains_key(&artist.id);
        if !ranked {
            seen_unranked = true;
        } else {
            assert!(
                !seen_unranked,
                "a ranked artist appeared after an unranked one"
            );
        }
    }
}
