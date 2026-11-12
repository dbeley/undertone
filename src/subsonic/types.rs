//! Serde types for the Subsonic API subset Undertone uses.
//!
//! Shapes were captured from a real Navidrome 0.64.2 (OpenSubsonic) instance.
//! Two rules learned from that server:
//!
//! 1. Every collection is `#[serde(default)]` — Navidrome omits empty arrays
//!    entirely, e.g. `"searchResult3": {}` for a query with no hits.
//! 2. Every scalar that the spec marks optional is `Option`, because
//!    OpenSubsonic servers vary in what they include.
#![allow(dead_code)] // fields are consumed by later phases

use serde::Deserialize;

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Album {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub song_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub play_count: Option<u64>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub song: Vec<Song>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Song {
    pub id: String,
    #[serde(default)]
    pub parent: Option<String>,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub album: Option<String>,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub artist_id: Option<String>,
    #[serde(default)]
    pub album_id: Option<String>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub track: Option<u32>,
    #[serde(default)]
    pub disc_number: Option<u32>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(default)]
    pub suffix: Option<String>,
    #[serde(default)]
    pub content_type: Option<String>,
    #[serde(default)]
    pub size: Option<u64>,
    #[serde(default)]
    pub bit_rate: Option<u32>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub genre: Option<String>,
    #[serde(default)]
    pub play_count: Option<u64>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub user_rating: Option<u32>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub replay_gain: Option<ReplayGain>,
    /// Subsonic's `type` field (`music`, `podcast`, …).
    #[serde(rename = "type", default)]
    pub media_kind: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayGain {
    #[serde(default)]
    pub track_gain: Option<f64>,
    #[serde(default)]
    pub album_gain: Option<f64>,
    #[serde(default)]
    pub track_peak: Option<f64>,
    #[serde(default)]
    pub album_peak: Option<f64>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artist {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub album_count: Option<u32>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub artist_image_url: Option<String>,
    #[serde(default)]
    pub music_brainz_id: Option<String>,
}

/// One letter of the artist index (`artists.index[]`).
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistIndex {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub artist: Vec<Artist>,
}

/// Response payload of `getArtists`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistsIndex {
    #[serde(default)]
    pub ignored_articles: String,
    #[serde(default)]
    pub index: Vec<ArtistIndex>,
}

impl ArtistsIndex {
    /// Flat iteration over every artist, in server index order.
    pub fn artists(&self) -> impl Iterator<Item = &Artist> {
        self.index.iter().flat_map(|idx| idx.artist.iter())
    }
}

/// Response payload of `getArtist`: an artist plus their albums.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistDetail {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub album_count: Option<u32>,
    #[serde(default)]
    pub starred: Option<String>,
    #[serde(default)]
    pub artist_image_url: Option<String>,
    #[serde(default)]
    pub album: Vec<Album>,
}

/// Response payload of `getArtistInfo2`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ArtistInfo {
    #[serde(default)]
    pub biography: Option<String>,
    #[serde(default)]
    pub music_brainz_id: Option<String>,
    #[serde(default)]
    pub last_fm_url: Option<String>,
    #[serde(default)]
    pub small_image_url: Option<String>,
    #[serde(default)]
    pub medium_image_url: Option<String>,
    #[serde(default)]
    pub large_image_url: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlist {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub song_count: Option<u32>,
    #[serde(default)]
    pub duration: Option<u32>,
    #[serde(rename = "public", default)]
    pub is_public: bool,
    #[serde(default)]
    pub owner: Option<String>,
    #[serde(default)]
    pub created: Option<String>,
    #[serde(default)]
    pub changed: Option<String>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub readonly: bool,
    #[serde(default)]
    pub entry: Vec<Song>,
}

/// `getAlbumList2` payload.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AlbumList {
    #[serde(default)]
    pub album: Vec<Album>,
}

/// Payload wrapper used by `getRandomSongs`, `getSimilarSongs2`, `getTopSongs`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SongList {
    #[serde(default)]
    pub song: Vec<Song>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SearchResult {
    #[serde(default)]
    pub artist: Vec<Artist>,
    #[serde(default)]
    pub album: Vec<Album>,
    #[serde(default)]
    pub song: Vec<Song>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Starred {
    #[serde(default)]
    pub artist: Vec<Artist>,
    #[serde(default)]
    pub album: Vec<Album>,
    #[serde(default)]
    pub song: Vec<Song>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Playlists {
    #[serde(default)]
    pub playlist: Vec<Playlist>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALBUM_LIST: &str = include_str!("../../tests/fixtures/albumList2.json");
    const ALBUM: &str = include_str!("../../tests/fixtures/album.json");
    const ARTISTS: &str = include_str!("../../tests/fixtures/artists.json");
    const ARTIST: &str = include_str!("../../tests/fixtures/artist.json");
    const ARTIST_INFO: &str = include_str!("../../tests/fixtures/artistInfo2.json");
    const PLAYLISTS: &str = include_str!("../../tests/fixtures/playlists.json");
    const PLAYLIST: &str = include_str!("../../tests/fixtures/playlist.json");
    const SEARCH: &str = include_str!("../../tests/fixtures/search3.json");
    const STARRED: &str = include_str!("../../tests/fixtures/starred2.json");
    const SIMILAR: &str = include_str!("../../tests/fixtures/similarSongs2.json");
    const RANDOM: &str = include_str!("../../tests/fixtures/randomSongs.json");

    /// Pull one payload key out of a captured envelope.
    fn payload(json: &str, key: &str) -> serde_json::Value {
        let value: serde_json::Value = serde_json::from_str(json).unwrap();
        value["subsonic-response"][key].clone()
    }

    #[test]
    fn album_list_parses() {
        let list: AlbumList = serde_json::from_value(payload(ALBUM_LIST, "albumList2")).unwrap();
        assert!(!list.album.is_empty());
        let first = &list.album[0];
        assert!(!first.id.is_empty());
        assert!(!first.name.is_empty());
        assert!(first.artist.is_some());
    }

    #[test]
    fn album_with_songs_parses() {
        let album: Album = serde_json::from_value(payload(ALBUM, "album")).unwrap();
        assert!(!album.song.is_empty());
        let song = &album.song[0];
        assert!(!song.id.is_empty());
        assert!(!song.title.is_empty());
        assert!(song.duration.unwrap_or(0) > 0);
        assert!(song.album_id.is_some());
        assert!(song.cover_art.is_some());
        assert!(song.replay_gain.is_some());
        assert_eq!(song.media_kind.as_deref(), Some("music"));
    }

    #[test]
    fn artists_index_parses_and_flattens() {
        let index: ArtistsIndex = serde_json::from_value(payload(ARTISTS, "artists")).unwrap();
        assert_eq!(index.ignored_articles, "The El La Los Las Le Les Os As O A");
        assert!(!index.index.is_empty());
        assert!(index.index.iter().any(|i| i.name == "#"));
        let total: usize = index.index.iter().map(|i| i.artist.len()).sum();
        assert_eq!(index.artists().count(), total);
        assert!(total > 0);
    }

    #[test]
    fn artist_detail_parses() {
        let artist: ArtistDetail = serde_json::from_value(payload(ARTIST, "artist")).unwrap();
        assert!(!artist.name.is_empty());
        assert!(artist.album_count.unwrap_or(0) > 0);
        assert!(!artist.album.is_empty());
    }

    #[test]
    fn artist_info_parses() {
        let info: ArtistInfo = serde_json::from_value(payload(ARTIST_INFO, "artistInfo2")).unwrap();
        assert!(info.biography.is_some());
    }

    #[test]
    fn playlists_parse() {
        let playlists: Playlists = serde_json::from_value(payload(PLAYLISTS, "playlists")).unwrap();
        assert!(!playlists.playlist.is_empty());
        let first = &playlists.playlist[0];
        assert!(!first.id.is_empty());
        assert!(first.song_count.unwrap_or(0) > 0);
    }

    #[test]
    fn playlist_with_entries_parses() {
        let playlist: Playlist = serde_json::from_value(payload(PLAYLIST, "playlist")).unwrap();
        assert!(!playlist.entry.is_empty());
        assert!(!playlist.entry[0].title.is_empty());
    }

    #[test]
    fn empty_payloads_parse_as_empty_collections() {
        // Navidrome returns `{}` rather than empty arrays.
        let search: SearchResult =
            serde_json::from_value(payload(SEARCH, "searchResult3")).unwrap();
        assert!(search.album.is_empty());
        assert!(search.artist.is_empty());
        assert!(search.song.is_empty());

        let random: SongList = serde_json::from_value(payload(RANDOM, "randomSongs")).unwrap();
        assert!(!random.song.is_empty());
    }

    #[test]
    fn starred_and_similar_parse() {
        let starred: Starred = serde_json::from_value(payload(STARRED, "starred2")).unwrap();
        assert!(!starred.album.is_empty());
        let similar: SongList = serde_json::from_value(payload(SIMILAR, "similarSongs2")).unwrap();
        assert!(!similar.song.is_empty());
    }

    #[test]
    fn missing_optional_fields_do_not_fail() {
        // A minimal song, as an old Subsonic server might return it.
        let song: Song = serde_json::from_str(r#"{"id":"1","title":"x"}"#).unwrap();
        assert_eq!(song.id, "1");
        assert!(song.artist.is_none());
        assert!(song.duration.is_none());
    }
}
