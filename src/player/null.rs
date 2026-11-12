//! A player that does nothing but record what it was asked to do.
//!
//! Used by tests, by the perf harness, and by `--player=null` so the app can
//! run on a machine without audio.

use super::{LoadMode, Player, PlayerError, PlayerEvent, Seek, TrackSource};

#[derive(Debug, Default)]
pub struct NullPlayer {
    /// Every track passed to `load`, in order.
    pub loaded: Vec<TrackSource>,
    pub paused: bool,
    pub volume: u8,
    pub muted: bool,
    pub seeks: Vec<(Seek, f64)>,
    pub steps: i32,
    pub stopped: bool,
    pub shutdown_called: bool,
    events: Vec<PlayerEvent>,
    /// Duration reported for each loaded track, for deterministic tests.
    pub reported_duration: f64,
}

impl NullPlayer {
    pub fn new() -> Self {
        Self {
            volume: 100,
            reported_duration: 200.0,
            ..Default::default()
        }
    }

    pub fn push_event(&mut self, event: PlayerEvent) {
        self.events.push(event);
    }
}

impl Player for NullPlayer {
    fn load(&mut self, tracks: &[TrackSource], mode: LoadMode) -> Result<(), PlayerError> {
        match mode {
            LoadMode::Replace => {
                self.loaded.clear();
                self.events.push(PlayerEvent::FileLoaded);
                self.events
                    .push(PlayerEvent::Duration(self.reported_duration));
            }
            LoadMode::Append => {}
        }
        self.loaded.extend(tracks.iter().cloned());
        Ok(())
    }

    fn play(&mut self) -> Result<(), PlayerError> {
        self.paused = false;
        self.events.push(PlayerEvent::Paused(false));
        Ok(())
    }

    fn pause(&mut self, paused: bool) -> Result<(), PlayerError> {
        self.paused = paused;
        self.events.push(PlayerEvent::Paused(paused));
        Ok(())
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.stopped = true;
        self.events.push(PlayerEvent::Ended(super::EndReason::Stop));
        Ok(())
    }

    fn seek(&mut self, mode: Seek, seconds: f64) -> Result<(), PlayerError> {
        self.seeks.push((mode, seconds));
        Ok(())
    }

    fn set_volume(&mut self, percent: u8) -> Result<(), PlayerError> {
        self.volume = percent.min(100);
        self.events
            .push(PlayerEvent::Volume(f64::from(self.volume)));
        Ok(())
    }

    fn set_mute(&mut self, muted: bool) -> Result<(), PlayerError> {
        self.muted = muted;
        Ok(())
    }

    fn next(&mut self) -> Result<(), PlayerError> {
        self.steps += 1;
        Ok(())
    }

    fn previous(&mut self) -> Result<(), PlayerError> {
        self.steps -= 1;
        Ok(())
    }

    fn drain_events(&mut self) -> Vec<PlayerEvent> {
        std::mem::take(&mut self.events)
    }

    fn shutdown(&mut self) {
        self.shutdown_called = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(title: &str) -> TrackSource {
        TrackSource {
            url: format!("https://x.test/stream?id={title}"),
            title: title.to_string(),
        }
    }

    #[test]
    fn replace_clears_previous_tracks() {
        let mut player = NullPlayer::new();
        player
            .load(&[track("a"), track("b")], LoadMode::Replace)
            .unwrap();
        player.load(&[track("c")], LoadMode::Replace).unwrap();
        assert_eq!(player.loaded.len(), 1);
        assert_eq!(player.loaded[0].title, "c");
    }

    #[test]
    fn append_keeps_previous_tracks() {
        let mut player = NullPlayer::new();
        player.load(&[track("a")], LoadMode::Replace).unwrap();
        player.load(&[track("b")], LoadMode::Append).unwrap();
        let titles: Vec<&str> = player.loaded.iter().map(|t| t.title.as_str()).collect();
        assert_eq!(titles, vec!["a", "b"]);
    }

    #[test]
    fn reports_duration_and_pause_events() {
        let mut player = NullPlayer::new();
        player.reported_duration = 123.0;
        player.load(&[track("a")], LoadMode::Replace).unwrap();
        let events = player.drain_events();
        assert!(events.contains(&PlayerEvent::FileLoaded));
        assert!(events.contains(&PlayerEvent::Duration(123.0)));
        player.play().unwrap();
        assert_eq!(player.drain_events(), vec![PlayerEvent::Paused(false)]);
    }

    #[test]
    fn records_seeks_and_volume() {
        let mut player = NullPlayer::new();
        player.seek(Seek::Relative, 10.0).unwrap();
        player.seek(Seek::Absolute, 42.0).unwrap();
        player.set_volume(55).unwrap();
        assert_eq!(
            player.seeks,
            vec![(Seek::Relative, 10.0), (Seek::Absolute, 42.0)]
        );
        assert_eq!(player.volume, 55);
    }
}
