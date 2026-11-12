//! Scrobbling rules, kept as a pure state machine.
//!
//! Subsonic's `scrobble` endpoint takes `submission=false` for "now playing"
//! and `submission=true` for an actual scrobble. The standard rule is 50 %
//! elapsed or 240 seconds, whichever comes first. Navidrome relays to
//! Last.fm/ListenBrainz server-side, so no API keys live here.

/// Fraction of the track required before submitting.
pub const SCROBBLE_RATIO: f64 = 0.5;
/// Absolute ceiling before submitting, in seconds.
pub const SCROBBLE_MAX_SECS: f64 = 240.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrobbleAction {
    /// `submission=false` at track start.
    NowPlaying { id: String },
    /// `submission=true` once the threshold is reached.
    Submit { id: String, started_at_ms: u64 },
}

#[derive(Debug, Clone)]
struct Active {
    id: String,
    started_at_ms: u64,
    submitted: bool,
}

#[derive(Debug, Clone)]
pub struct Scrobbler {
    ratio: f64,
    max_secs: f64,
    active: Option<Active>,
}

impl Default for Scrobbler {
    fn default() -> Self {
        Self::new(SCROBBLE_RATIO, SCROBBLE_MAX_SECS)
    }
}

impl Scrobbler {
    pub fn new(ratio: f64, max_secs: f64) -> Self {
        Self {
            ratio,
            max_secs,
            active: None,
        }
    }

    /// Begin a play. Always emits "now playing" and never submits for the
    /// previous track.
    pub fn start(&mut self, id: &str, started_at_ms: u64) -> Vec<ScrobbleAction> {
        self.active = Some(Active {
            id: id.to_string(),
            started_at_ms,
            submitted: false,
        });
        vec![ScrobbleAction::NowPlaying { id: id.to_string() }]
    }

    /// Report accumulated playback time.
    ///
    /// `listened_secs` must be time actually played, not the seek position:
    /// jumping to 90 % of a track should not scrobble it.
    pub fn progress(
        &mut self,
        listened_secs: f64,
        duration_secs: Option<f64>,
    ) -> Vec<ScrobbleAction> {
        let threshold = self.threshold(duration_secs);
        let Some(active) = self.active.as_mut() else {
            return Vec::new();
        };
        if active.submitted {
            return Vec::new();
        }
        if listened_secs < threshold {
            return Vec::new();
        }
        active.submitted = true;
        vec![ScrobbleAction::Submit {
            id: active.id.clone(),
            started_at_ms: active.started_at_ms,
        }]
    }

    /// End the current play without submitting.
    pub fn stop(&mut self) {
        self.active = None;
    }

    /// Seconds required before submitting for a track of this length.
    pub fn threshold(&self, duration_secs: Option<f64>) -> f64 {
        match duration_secs {
            Some(duration) if duration > 0.0 => (duration * self.ratio).min(self.max_secs),
            // Unknown duration: fall back to the absolute ceiling.
            _ => self.max_secs,
        }
    }

    pub fn has_submitted(&self) -> bool {
        self.active.as_ref().is_some_and(|a| a.submitted)
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active.as_ref().map(|a| a.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn threshold_is_half_or_four_minutes_whichever_is_first() {
        let scrobbler = Scrobbler::default();
        // Short track: 50 % wins.
        assert_eq!(scrobbler.threshold(Some(200.0)), 100.0);
        // Long track: 240 s wins.
        assert_eq!(scrobbler.threshold(Some(600.0)), 240.0);
        // Exactly at the crossover.
        assert_eq!(scrobbler.threshold(Some(480.0)), 240.0);
        // Unknown duration.
        assert_eq!(scrobbler.threshold(None), 240.0);
        // Degenerate duration.
        assert_eq!(scrobbler.threshold(Some(0.0)), 240.0);
    }

    #[test]
    fn start_emits_now_playing_only() {
        let mut scrobbler = Scrobbler::default();
        assert_eq!(
            scrobbler.start("abc", 1_000),
            vec![ScrobbleAction::NowPlaying {
                id: "abc".to_string()
            }]
        );
        assert!(!scrobbler.has_submitted());
    }

    #[test]
    fn submits_once_at_the_threshold() {
        let mut scrobbler = Scrobbler::default();
        scrobbler.start("abc", 1_000);

        assert!(scrobbler.progress(99.0, Some(200.0)).is_empty());
        assert_eq!(
            scrobbler.progress(100.0, Some(200.0)),
            vec![ScrobbleAction::Submit {
                id: "abc".to_string(),
                started_at_ms: 1_000
            }]
        );
        // Never twice.
        assert!(scrobbler.progress(150.0, Some(200.0)).is_empty());
        assert!(scrobbler.progress(199.0, Some(200.0)).is_empty());
    }

    #[test]
    fn long_tracks_submit_at_four_minutes() {
        let mut scrobbler = Scrobbler::default();
        scrobbler.start("long", 5);
        assert!(scrobbler.progress(239.0, Some(900.0)).is_empty());
        assert_eq!(scrobbler.progress(240.0, Some(900.0)).len(), 1);
    }

    #[test]
    fn skipped_tracks_never_submit() {
        let mut scrobbler = Scrobbler::default();
        scrobbler.start("a", 1);
        assert!(scrobbler.progress(10.0, Some(300.0)).is_empty());
        // Track changes before the threshold.
        scrobbler.start("b", 2);
        assert!(scrobbler.progress(10.0, Some(300.0)).is_empty());
        assert_eq!(scrobbler.active_id(), Some("b"));
    }

    #[test]
    fn a_new_play_resets_the_submission_state() {
        let mut scrobbler = Scrobbler::default();
        scrobbler.start("a", 1);
        scrobbler.progress(200.0, Some(200.0));
        assert!(scrobbler.has_submitted());

        scrobbler.start("b", 2);
        assert!(!scrobbler.has_submitted());
        assert_eq!(scrobbler.active_id(), Some("b"));
        // Replaying the same track counts as a new play instance.
        scrobbler.start("a", 3);
        assert!(!scrobbler.has_submitted());
    }

    #[test]
    fn stop_prevents_a_late_submission() {
        let mut scrobbler = Scrobbler::default();
        scrobbler.start("a", 1);
        scrobbler.stop();
        assert!(scrobbler.progress(500.0, Some(200.0)).is_empty());
        assert_eq!(scrobbler.active_id(), None);
    }

    #[test]
    fn progress_without_a_play_is_a_no_op() {
        let mut scrobbler = Scrobbler::default();
        assert!(scrobbler.progress(500.0, Some(100.0)).is_empty());
    }
}
