//! Typed endpoint wrappers.
#![allow(dead_code)] // consumed by later phases

use super::client::{ServerInfo, SubsonicClient, SubsonicError};
use super::types::*;

/// `getAlbumList2` sorting modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlbumListType {
    Random,
    Newest,
    Frequent,
    Recent,
    AlphabeticalByName,
    AlphabeticalByArtist,
    Starred,
    ByYear,
    ByGenre,
}

impl AlbumListType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Newest => "newest",
            Self::Frequent => "frequent",
            Self::Recent => "recent",
            Self::AlphabeticalByName => "alphabeticalByName",
            Self::AlphabeticalByArtist => "alphabeticalByArtist",
            Self::Starred => "starred",
            Self::ByYear => "byYear",
            Self::ByGenre => "byGenre",
        }
    }

    /// Every mode the endpoint knows, for parsing a saved key.
    pub const ALL: [Self; 9] = [
        Self::Random,
        Self::Newest,
        Self::Frequent,
        Self::Recent,
        Self::AlphabeticalByName,
        Self::AlphabeticalByArtist,
        Self::Starred,
        Self::ByYear,
        Self::ByGenre,
    ];

    /// The inverse of `as_str`, for a saved session.
    pub fn from_key(key: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|mode| mode.as_str() == key)
    }

    /// Modes offered in the UI, in cycling order.
    ///
    /// Exactly what the list is likely to be wanted for: discovery, what is
    /// new, what gets played, and finding a name. Starred and by-year remain
    /// available in the endpoint but are no longer in the cycle — favourites
    /// have their own screen, and by-year is a filter more than a sort.
    pub const ALL_IN_UI: [Self; 4] = [
        Self::Random,
        Self::Newest,
        Self::Frequent,
        Self::AlphabeticalByName,
    ];

    /// Human label for the status line.
    pub fn label(self) -> &'static str {
        match self {
            Self::Random => "random",
            Self::Newest => "recent",
            Self::Frequent => "most played",
            Self::AlphabeticalByName => "a-z",
            Self::AlphabeticalByArtist => "artist a-z",
            Self::Recent => "recently played",
            Self::Starred => "starred",
            Self::ByYear => "year",
            Self::ByGenre => "genre",
        }
    }
}

/// Parameters for one page of `getAlbumList2`.
#[derive(Debug, Clone)]
pub struct AlbumListQuery {
    pub kind: AlbumListType,
    pub size: u32,
    pub offset: u32,
    pub genre: Option<String>,
    pub from_year: Option<u32>,
    pub to_year: Option<u32>,
}

impl Default for AlbumListQuery {
    fn default() -> Self {
        Self {
            kind: AlbumListType::Random,
            size: 50,
            offset: 0,
            genre: None,
            from_year: None,
            to_year: None,
        }
    }
}

/// Server limits `getAlbumList2` size to 500.
pub const MAX_ALBUM_PAGE: u32 = 500;

impl SubsonicClient {
    pub fn ping(&self) -> Result<ServerInfo, SubsonicError> {
        let response = self.envelope("ping", &[])?;
        Ok(ServerInfo {
            version: response.version.unwrap_or_else(|| "unknown".to_string()),
            server_type: response.server_type,
            open_subsonic: response.open_subsonic,
        })
    }

    pub fn album_list(&self, query: &AlbumListQuery) -> Result<Vec<Album>, SubsonicError> {
        let size = query.size.clamp(1, MAX_ALBUM_PAGE).to_string();
        let offset = query.offset.to_string();
        let mut params: Vec<(&str, &str)> = vec![
            ("type", query.kind.as_str()),
            ("size", size.as_str()),
            ("offset", offset.as_str()),
        ];
        if let Some(genre) = query.genre.as_deref() {
            params.push(("genre", genre));
        }
        let from = query.from_year.map(|y| y.to_string());
        if let Some(from) = from.as_deref() {
            params.push(("fromYear", from));
        }
        let to = query.to_year.map(|y| y.to_string());
        if let Some(to) = to.as_deref() {
            params.push(("toYear", to));
        }
        let list: AlbumList = self.get_json("getAlbumList2", "albumList2", &params)?;
        Ok(list.album)
    }

    pub fn album(&self, id: &str) -> Result<Album, SubsonicError> {
        self.get_json("getAlbum", "album", &[("id", id)])
    }

    pub fn artists(&self) -> Result<ArtistsIndex, SubsonicError> {
        self.get_json("getArtists", "artists", &[])
    }

    pub fn artist(&self, id: &str) -> Result<ArtistDetail, SubsonicError> {
        self.get_json("getArtist", "artist", &[("id", id)])
    }

    pub fn artist_info(&self, id: &str) -> Result<ArtistInfo, SubsonicError> {
        self.get_json("getArtistInfo2", "artistInfo2", &[("id", id)])
    }

    /// Songs similar to `id`.
    ///
    /// Uses `getSimilarSongs` (v1) rather than `getSimilarSongs2`: on Navidrome
    /// 0.64 the v2 endpoint stalled past a thirty-second read timeout while v1
    /// answered immediately, and hanging the UI to build a queue is a far worse
    /// failure than a thinner list.
    pub fn similar_songs(&self, id: &str, count: u32) -> Result<Vec<Song>, SubsonicError> {
        let count = count.to_string();
        let list: SongList = self.get_json(
            "getSimilarSongs",
            "similarSongs",
            &[("id", id), ("count", &count)],
        )?;
        Ok(list.song)
    }

    /// The OpenSubsonic `getSimilarSongs2` variant, kept for servers that only
    /// implement the newer form. Not used on the hot path: see above.
    pub fn similar_songs_v2(&self, id: &str, count: u32) -> Result<Vec<Song>, SubsonicError> {
        let count = count.to_string();
        let list: SongList = self.get_json(
            "getSimilarSongs2",
            "similarSongs2",
            &[("id", id), ("count", &count)],
        )?;
        Ok(list.song)
    }

    pub fn top_songs(&self, artist: &str, count: u32) -> Result<Vec<Song>, SubsonicError> {
        let count = count.to_string();
        let list: SongList = self.get_json(
            "getTopSongs",
            "topSongs",
            &[("artist", artist), ("count", &count)],
        )?;
        Ok(list.song)
    }

    pub fn random_songs(&self, size: u32) -> Result<Vec<Song>, SubsonicError> {
        let size = size.to_string();
        let list: SongList = self.get_json("getRandomSongs", "randomSongs", &[("size", &size)])?;
        Ok(list.song)
    }

    pub fn search(&self, query: &str) -> Result<SearchResult, SubsonicError> {
        self.get_json(
            "search3",
            "searchResult3",
            &[
                ("query", query),
                ("songCount", "50"),
                ("albumCount", "20"),
                ("artistCount", "20"),
            ],
        )
    }

    pub fn playlists(&self) -> Result<Vec<Playlist>, SubsonicError> {
        let playlists: Playlists = self.get_json("getPlaylists", "playlists", &[])?;
        Ok(playlists.playlist)
    }

    pub fn playlist(&self, id: &str) -> Result<Playlist, SubsonicError> {
        self.get_json("getPlaylist", "playlist", &[("id", id)])
    }

    pub fn starred(&self) -> Result<Starred, SubsonicError> {
        self.get_json("getStarred2", "starred2", &[])
    }

    pub fn star(&self, id: &str) -> Result<(), SubsonicError> {
        self.envelope("star", &[("id", id)]).map(|_| ())
    }

    pub fn unstar(&self, id: &str) -> Result<(), SubsonicError> {
        self.envelope("unstar", &[("id", id)]).map(|_| ())
    }

    /// Report a play. `submission = false` is "now playing"; `true` is a scrobble.
    pub fn scrobble(
        &self,
        id: &str,
        submission: bool,
        time_ms: Option<u64>,
    ) -> Result<(), SubsonicError> {
        let submission = if submission { "true" } else { "false" };
        let time = time_ms.map(|t| t.to_string());
        let mut params: Vec<(&str, &str)> = vec![("id", id), ("submission", submission)];
        if let Some(time) = time.as_deref() {
            params.push(("time", time));
        }
        self.envelope("scrobble", &params).map(|_| ())
    }

    /// Cover art bytes and their content type. Navidrome defaults to webp.
    pub fn cover_art(
        &self,
        id: &str,
        size: Option<u32>,
    ) -> Result<(Vec<u8>, String), SubsonicError> {
        let size = size.map(|s| s.to_string());
        let mut params: Vec<(&str, &str)> = vec![("id", id)];
        if let Some(size) = size.as_deref() {
            params.push(("size", size));
        }
        self.get_bytes("getCoverArt", &params)
    }

    /// A stream URL, authenticated at call time.
    ///
    /// `max_bit_rate` / `format` request server-side transcoding; pass `None`
    /// for the original file.
    pub fn stream_url(&self, id: &str, max_bit_rate: Option<u32>, format: Option<&str>) -> String {
        let bitrate = max_bit_rate.map(|b| b.to_string());
        let mut params: Vec<(&str, &str)> = vec![("id", id)];
        if let Some(bitrate) = bitrate.as_deref() {
            params.push(("maxBitRate", bitrate));
        }
        if let Some(format) = format {
            params.push(("format", format));
        }
        self.build_url("stream", &params)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn album_list_types_serialise_to_spec_names() {
        assert_eq!(AlbumListType::Random.as_str(), "random");
        assert_eq!(AlbumListType::Newest.as_str(), "newest");
        assert_eq!(
            AlbumListType::AlphabeticalByArtist.as_str(),
            "alphabeticalByArtist"
        );
        assert_eq!(AlbumListType::ByYear.as_str(), "byYear");
        assert_eq!(AlbumListType::Starred.as_str(), "starred");
    }

    #[test]
    fn every_ui_mode_has_a_spec_name() {
        for kind in AlbumListType::ALL_IN_UI {
            assert!(!kind.as_str().is_empty());
        }
    }

    #[test]
    fn album_page_size_is_clamped_to_server_limit() {
        let query = AlbumListQuery {
            size: 10_000,
            ..Default::default()
        };
        assert_eq!(query.size.clamp(1, MAX_ALBUM_PAGE), 500);
    }

    #[test]
    fn stream_url_without_transcode_has_no_format_param() {
        let client = SubsonicClient::new(super::super::SubsonicConfig {
            server: "https://x.test".into(),
            username: "u".into(),
            password: "p".into(),
        });
        let url = client.stream_url("abc", None, None);
        assert!(url.contains("/rest/stream.view?"));
        assert!(url.contains("id=abc"));
        assert!(!url.contains("format="));
        assert!(!url.contains("maxBitRate="));
    }

    #[test]
    fn stream_url_with_transcode_carries_params() {
        let client = SubsonicClient::new(super::super::SubsonicConfig {
            server: "https://x.test".into(),
            username: "u".into(),
            password: "p".into(),
        });
        let url = client.stream_url("abc", Some(192), Some("mp3"));
        assert!(url.contains("maxBitRate=192"));
        assert!(url.contains("format=mp3"));
    }
}
