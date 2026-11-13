//! What was playing when the app closed.
//!
//! Saved to the state directory beside listen later. Only the queue and the
//! cursor are kept: the stream is not resumed on start, so opening the app is
//! silent until you ask for sound.
//!
//! This restores *what* was queued, not *why*. Shuffle order is not persisted,
//! so a shuffled queue comes back in its stored order; that is honest about
//! what a small file can promise.

use std::fs;
use std::io;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::queue::QueueItem;

/// Shape of the file this build writes.
///
/// A session written by a newer build is ignored rather than half-read: a
/// queue is a convenience, and guessing at fields we do not know is worse than
/// starting empty. Files from before this field existed read as version 0.
pub const VERSION: u32 = 1;

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub version: u32,
    #[serde(default)]
    pub queue: Vec<QueueItem>,
    /// Position within the queue as it was saved.
    #[serde(default)]
    pub position: usize,
    #[serde(default)]
    pub volume: Option<u8>,
    /// How far into the current track it was.
    #[serde(default)]
    pub seconds: f64,
    /// How the lists were sorted and laid out. Kept here rather than in the
    /// config because they are workspace state: you set them by pressing S and
    /// w, not by editing a file.
    #[serde(default)]
    pub sort: Option<String>,
    #[serde(default)]
    pub artist_sort: Option<String>,
    #[serde(default)]
    pub layout: Option<String>,
    #[serde(default)]
    pub starred_layout: Option<String>,
    #[serde(default)]
    pub later_layout: Option<String>,
    /// Which list was open, as a stable key. Detail views are not stored: an
    /// album detail needs the album, and reloading it on start would mean a
    /// network round trip before the first frame.
    #[serde(default)]
    pub view: Option<String>,
}

impl Session {
    /// True when the file carries no state at all.
    pub fn is_empty(&self) -> bool {
        self.queue.is_empty()
            && self.view.is_none()
            && self.sort.is_none()
            && self.artist_sort.is_none()
            && self.layout.is_none()
    }

    /// Load, or `None` when there is nothing usable to restore.
    ///
    /// A corrupt or half-written file is treated as no session at all: losing a
    /// queue is an annoyance, refusing to start is not acceptable.
    pub fn load(path: impl Into<PathBuf>) -> Option<Self> {
        let bytes = fs::read(path.into()).ok()?;
        let session: Session = serde_json::from_slice(&bytes).ok()?;
        if session.version > VERSION {
            return None;
        }
        // A file that says nothing is no session. Anything else is worth
        // reading even with an empty queue: the sort, the layout and the list
        // you were on are saved there too, and quitting without playing
        // anything must not throw those away.
        if session.is_empty() {
            return None;
        }
        Some(session)
    }

    pub fn save(&self, path: impl Into<PathBuf>) -> io::Result<()> {
        let mut this = self.clone();
        this.version = VERSION;
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(&this)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        // Temp file plus rename: an interrupted write must not leave a
        // truncated session behind for the next start to choke on.
        let temp = path.with_extension("json.tmp");
        fs::write(&temp, json)?;
        fs::rename(&temp, &path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("undertone-sess-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("session.json")
    }

    fn item(id: &str) -> QueueItem {
        QueueItem {
            id: id.to_string(),
            title: format!("track {id}"),
            artist: Some("an artist".into()),
            album: Some("an album".into()),
            duration: Some(200),
            suffix: Some("flac".into()),
            bit_rate: Some(900),
            ..Default::default()
        }
    }

    #[test]
    fn a_newer_file_is_ignored_rather_than_half_read() {
        let path = temp("newer");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // Simulate a file written by a later build.
        std::fs::write(
            &path,
            br#"{"version": 9, "queue": [{"id": "a", "title": "A"}]}"#,
        )
        .unwrap();
        assert!(Session::load(&path).is_none());
    }

    #[test]
    fn a_file_with_no_version_still_loads() {
        let path = temp("legacy");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        // The shape written before the field existed.
        std::fs::write(&path, br#"{"queue": [{"id": "a", "title": "A"}]}"#).unwrap();
        let session = Session::load(&path).expect("a legacy session is readable");
        assert_eq!(session.position, 0);
    }

    #[test]
    fn saving_stamps_the_current_version() {
        let path = temp("stamp");
        Session {
            queue: vec![item("a")],
            ..Default::default()
        }
        .save(&path)
        .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value["version"], super::VERSION);
    }

    #[test]
    fn a_saved_session_round_trips() {
        let path = temp("roundtrip");
        let session = Session {
            queue: vec![item("a"), item("b")],
            position: 1,
            volume: Some(65),
            seconds: 63.5,
            sort: Some("alphabeticalByName".into()),
            artist_sort: Some("a-z".into()),
            layout: Some("grid".into()),
            view: Some("queue".into()),
            ..Default::default()
        };
        session.save(&path).unwrap();
        let stamped = Session {
            version: VERSION,
            ..session.clone()
        };

        let loaded = Session::load(&path).expect("session");
        assert_eq!(loaded, stamped, "saving stamps the version");
        assert_eq!(loaded.queue[1].bit_rate, Some(900), "format survives");
        assert_eq!(loaded.sort.as_deref(), Some("alphabeticalByName"));
        assert_eq!(loaded.layout.as_deref(), Some("grid"));
    }

    #[test]
    fn an_empty_queue_is_not_a_session() {
        let path = temp("empty");
        Session::default().save(&path).unwrap();
        assert!(Session::load(&path).is_none(), "nothing worth restoring");
    }

    #[test]
    fn a_corrupt_file_is_no_session_rather_than_a_crash() {
        let path = temp("corrupt");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"{ truncated").unwrap();
        assert!(Session::load(&path).is_none());
    }

    #[test]
    fn a_missing_file_is_no_session() {
        assert!(Session::load(temp("missing")).is_none());
    }

    #[test]
    fn a_position_past_the_end_is_not_fatal() {
        let path = temp("position");
        let session = Session {
            queue: vec![item("a")],
            position: 42,
            volume: None,
            ..Default::default()
        };
        session.save(&path).unwrap();
        // Loading keeps the stored value; the app clamps when applying it.
        assert_eq!(Session::load(&path).unwrap().position, 42);
    }
}
