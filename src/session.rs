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

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Session {
    #[serde(default)]
    pub queue: Vec<QueueItem>,
    /// Position within the queue as it was saved.
    #[serde(default)]
    pub position: usize,
    #[serde(default)]
    pub volume: Option<u8>,
}

impl Session {
    /// Load, or `None` when there is nothing usable to restore.
    ///
    /// A corrupt or half-written file is treated as no session at all: losing a
    /// queue is an annoyance, refusing to start is not acceptable.
    pub fn load(path: impl Into<PathBuf>) -> Option<Self> {
        let bytes = fs::read(path.into()).ok()?;
        let session: Session = serde_json::from_slice(&bytes).ok()?;
        if session.queue.is_empty() {
            return None;
        }
        Some(session)
    }

    pub fn save(&self, path: impl Into<PathBuf>) -> io::Result<()> {
        let path = path.into();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
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
    fn a_saved_session_round_trips() {
        let path = temp("roundtrip");
        let session = Session {
            queue: vec![item("a"), item("b")],
            position: 1,
            volume: Some(65),
        };
        session.save(&path).unwrap();

        let loaded = Session::load(&path).expect("session");
        assert_eq!(loaded, session);
        assert_eq!(loaded.queue[1].bit_rate, Some(900), "format survives");
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
        };
        session.save(&path).unwrap();
        // Loading keeps the stored value; the app clamps when applying it.
        assert_eq!(Session::load(&path).unwrap().position, 42);
    }
}
