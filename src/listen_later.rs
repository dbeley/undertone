//! Albums kept for later, persisted as JSON.
//!
//! Covertone keeps this in `localStorage`; with no browser there is a file in
//! the state directory. Semantics match: newest first, adding twice is a no-op,
//! and entries are stored whole so the list renders without a server round trip.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::subsonic::Album;

/// One saved album.
///
/// The fields are copied rather than the whole `Album`: the stored form is a
/// cache of what the list needs to draw, and keeping it narrow means a server
/// field changing shape cannot corrupt the file.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub artist: Option<String>,
    #[serde(default)]
    pub cover_art: Option<String>,
    #[serde(default)]
    pub year: Option<i32>,
    #[serde(default)]
    pub song_count: Option<u32>,
    /// The album's star, copied at save time and kept in step afterwards.
    ///
    /// Without it the listen later page would be the one place a star could not
    /// be seen: its rows are rebuilt from this file, not from the server.
    #[serde(default)]
    pub starred: Option<String>,
    /// Unix milliseconds; the list is ordered by this, newest first.
    #[serde(default)]
    pub added_at: u64,
}

impl Entry {
    pub fn from_album(album: &Album) -> Self {
        Self {
            id: album.id.clone(),
            name: album.name.clone(),
            artist: album.artist.clone(),
            cover_art: album.cover_art.clone(),
            year: album.year,
            song_count: album.song_count,
            starred: album.starred.clone(),
            added_at: now_ms(),
        }
    }

    /// Back to an `Album` so the existing row renderer can draw it.
    pub fn to_album(&self) -> Album {
        Album {
            id: self.id.clone(),
            name: self.name.clone(),
            artist: self.artist.clone(),
            cover_art: self.cover_art.clone(),
            year: self.year,
            song_count: self.song_count,
            starred: self.starred.clone(),
            ..Default::default()
        }
    }
}

pub struct ListenLater {
    path: PathBuf,
    entries: Vec<Entry>,
}

impl ListenLater {
    /// Load from disk, or start empty. A corrupt file is ignored rather than
    /// fatal: losing a convenience list should never stop playback.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let entries = fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<Entry>>(&bytes).ok())
            .unwrap_or_default();
        Self { path, entries }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn albums(&self) -> Vec<Album> {
        self.entries.iter().map(Entry::to_album).collect()
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn contains(&self, album_id: &str) -> bool {
        self.entries.iter().any(|entry| entry.id == album_id)
    }

    /// Add if absent, remove if present. Returns true when the album is now
    /// saved, which is what the status message reports.
    pub fn toggle(&mut self, album: &Album) -> io::Result<bool> {
        let saved = if let Some(index) = self.entries.iter().position(|e| e.id == album.id) {
            self.entries.remove(index);
            false
        } else {
            self.entries.insert(0, Entry::from_album(album));
            true
        };
        self.save()?;
        Ok(saved)
    }

    pub fn remove(&mut self, album_id: &str) -> io::Result<()> {
        self.entries.retain(|entry| entry.id != album_id);
        self.save()
    }

    /// Keep a saved album's star in step with the server.
    ///
    /// Writes nothing when the album is not on the list: starring something in
    /// the library should not rewrite a file that never mentions it.
    pub fn set_starred(&mut self, album_id: &str, starred: Option<String>) -> io::Result<()> {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == album_id) else {
            return Ok(());
        };
        entry.starred = starred;
        self.save()
    }

    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        self.save()
    }

    fn save(&self) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&self.entries)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // Write beside the target and rename, so a crash mid-write cannot
        // leave a truncated list behind.
        let temp = self.path.with_extension("json.tmp");
        fs::write(&temp, json)?;
        fs::rename(&temp, &self.path)
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("undertone-ll-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("listen_later.json")
    }

    fn album(id: &str) -> Album {
        Album {
            id: id.to_string(),
            name: format!("Album {id}"),
            artist: Some("an artist".into()),
            cover_art: Some(format!("al-{id}")),
            year: Some(2020),
            ..Default::default()
        }
    }

    #[test]
    fn toggling_adds_then_removes_and_persists_across_loads() {
        let path = temp("toggle");
        let mut list = ListenLater::load(&path);
        assert!(list.is_empty());

        assert!(list.toggle(&album("a")).unwrap(), "first toggle saves");
        assert!(list.contains("a"));
        assert_eq!(list.len(), 1);

        // Reload from disk: the entry survived.
        let reloaded = ListenLater::load(&path);
        assert_eq!(reloaded.len(), 1);
        assert!(reloaded.contains("a"));

        let mut list = reloaded;
        assert!(!list.toggle(&album("a")).unwrap(), "second toggle removes");
        assert!(!list.contains("a"));
        assert!(ListenLater::load(&path).is_empty());
    }

    #[test]
    fn newest_first_order_is_kept() {
        let path = temp("order");
        let mut list = ListenLater::load(&path);
        list.toggle(&album("a")).unwrap();
        list.toggle(&album("b")).unwrap();
        list.toggle(&album("c")).unwrap();
        let names: Vec<&str> = list.entries().iter().map(|e| e.id.as_str()).collect();
        assert_eq!(names, vec!["c", "b", "a"]);
    }

    #[test]
    fn adding_the_same_album_twice_is_a_removal_not_a_duplicate() {
        let path = temp("dup");
        let mut list = ListenLater::load(&path);
        list.toggle(&album("a")).unwrap();
        list.toggle(&album("a")).unwrap();
        assert!(list.is_empty());
    }

    #[test]
    fn a_corrupt_file_is_ignored_rather_than_fatal() {
        let path = temp("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{not json").unwrap();
        let list = ListenLater::load(&path);
        assert!(list.is_empty(), "a broken list must not stop the app");
    }

    #[test]
    fn entries_round_trip_into_renderable_albums() {
        let path = temp("roundtrip");
        let mut list = ListenLater::load(&path);
        list.toggle(&album("a")).unwrap();
        let albums = list.albums();
        assert_eq!(albums.len(), 1);
        assert_eq!(albums[0].name, "Album a");
        assert_eq!(albums[0].cover_art.as_deref(), Some("al-a"));
    }

    #[test]
    fn a_star_survives_the_file_and_the_reload() {
        let path = temp("starred");
        let mut list = ListenLater::load(&path);
        list.toggle(&album("a")).unwrap();
        list.toggle(&album("b")).unwrap();

        list.set_starred("a", Some("now".into())).unwrap();
        let reloaded = ListenLater::load(&path);
        let albums = reloaded.albums();
        let starred = |id: &str| {
            albums
                .iter()
                .find(|album| album.id == id)
                .and_then(|album| album.starred.clone())
        };
        assert_eq!(starred("a").as_deref(), Some("now"));
        assert_eq!(starred("b"), None);

        // An album that was never saved is not worth a write.
        let mut list = reloaded;
        let before = fs::read(&path).unwrap();
        list.set_starred("elsewhere", Some("now".into())).unwrap();
        assert_eq!(fs::read(&path).unwrap(), before);
    }

    #[test]
    fn an_entry_saved_from_a_starred_album_keeps_its_star() {
        let path = temp("starred-on-save");
        let mut list = ListenLater::load(&path);
        let mut starred = album("a");
        starred.starred = Some("2020-01-01".into());
        list.toggle(&starred).unwrap();
        assert_eq!(
            ListenLater::load(&path).albums()[0].starred.as_deref(),
            Some("2020-01-01")
        );
    }

    #[test]
    fn remove_and_clear_write_through() {
        let path = temp("remove");
        let mut list = ListenLater::load(&path);
        list.toggle(&album("a")).unwrap();
        list.toggle(&album("b")).unwrap();
        list.remove("a").unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(ListenLater::load(&path).len(), 1);
        list.clear().unwrap();
        assert!(ListenLater::load(&path).is_empty());
    }
}
