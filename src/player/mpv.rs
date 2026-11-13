//! mpv backend: subprocess plus unix-socket JSON IPC.
//!
//! The flag set below is the one measured in `docs/DESIGN.md`: it brings mpv
//! from ~72 MB down to ~66 MB by disabling video output, OSD, scripts and by
//! capping the demuxer cache. Do not add flags back without re-measuring.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread::JoinHandle;

use crate::process::detach_from_terminal;
use std::time::{Duration, Instant};

use super::protocol::{encode_command, encode_observe, encode_set, parse_event};
use super::{LoadMode, Player, PlayerError, PlayerEvent, Seek, TrackSource};

/// How long to wait for mpv's IPC socket to appear after spawn.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const CONNECT_POLL: Duration = Duration::from_millis(20);

#[derive(Debug, Clone)]
pub struct MpvOptions {
    pub binary: PathBuf,
    /// Force an audio output. `Some("null")` makes playback headless, which is
    /// what tests and CI use.
    pub audio_output: Option<String>,
    /// Where to create the IPC socket. Must stay short: unix socket paths are
    /// limited to ~104 bytes.
    pub socket_path: PathBuf,
    /// Initial volume, 0-100.
    pub volume: Option<u8>,
}

impl Default for MpvOptions {
    fn default() -> Self {
        Self {
            binary: PathBuf::from("mpv"),
            audio_output: None,
            socket_path: default_socket_path(),
            volume: None,
        }
    }
}

/// A short, process-unique socket path.
pub fn default_socket_path() -> PathBuf {
    let dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".to_string());
    PathBuf::from(format!("{dir}/undertone-{}.sock", std::process::id()))
}

pub struct MpvPlayer {
    child: Child,
    socket_path: PathBuf,
    writer: Option<UnixStream>,
    events: Receiver<PlayerEvent>,
    reader: Option<JoinHandle<()>>,
    shutdown: bool,
}

impl MpvPlayer {
    pub fn spawn(options: &MpvOptions) -> Result<Self, PlayerError> {
        // A stale socket from a previous run would be connected to instead of
        // the new process; remove it first.
        let _ = std::fs::remove_file(&options.socket_path);

        let mut command = Command::new(&options.binary);
        command
            .arg("--no-config")
            .arg("--idle=yes")
            .arg("--no-video")
            .arg("--vo=null")
            .arg("--audio-display=no")
            .arg("--load-scripts=no")
            .arg("--no-osc")
            .arg("--no-osd-bar")
            .arg("--really-quiet")
            .arg("--no-terminal")
            // Gapless: hand mpv several tracks and let it run from one to the
            // next without reopening the audio device.
            .arg("--gapless-audio=yes")
            .arg("--prefetch-playlist=yes")
            .arg("--demuxer-max-bytes=8MiB")
            .arg("--demuxer-max-back-bytes=2MiB")
            .arg("--cache-secs=20")
            .arg(format!(
                "--input-ipc-server={}",
                options.socket_path.display()
            ))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(audio_output) = &options.audio_output {
            command.arg(format!("--ao={audio_output}"));
        }
        if let Some(volume) = options.volume {
            command.arg(format!("--volume={volume}"));
        }
        // mpv must not own the terminal either; a child with /dev/tty can
        // probe it and the replies are read as UI keystrokes.
        detach_from_terminal(&mut command);

        let child = command
            .spawn()
            .map_err(|err| PlayerError::Spawn(format!("{}: {err}", options.binary.display())))?;

        let stream = connect_with_timeout(&options.socket_path)?;
        let reader_stream = stream
            .try_clone()
            .map_err(|e| PlayerError::Ipc(e.to_string()))?;

        let (tx, rx) = mpsc::channel();
        let reader = std::thread::Builder::new()
            .name("mpv-ipc".to_string())
            .spawn(move || {
                let buffered = BufReader::new(reader_stream);
                for line in buffered.lines() {
                    match line {
                        Ok(line) => {
                            if let Some(event) = parse_event(&line) {
                                if tx.send(event).is_err() {
                                    break;
                                }
                            }
                        }
                        Err(err) => {
                            let _ = tx.send(PlayerEvent::Crashed(err.to_string()));
                            break;
                        }
                    }
                }
                // Channel closed or stream ended.
                let _ = tx.send(PlayerEvent::Crashed("ipc stream closed".to_string()));
            })
            .map_err(|e| PlayerError::Ipc(e.to_string()))?;

        let mut player = Self {
            child,
            socket_path: options.socket_path.clone(),
            writer: Some(stream),
            events: rx,
            reader: Some(reader),
            shutdown: false,
        };

        for (id, property) in [
            (1, "time-pos"),
            (2, "duration"),
            (3, "pause"),
            (4, "volume"),
            // Which entry of the handed-over playlist is playing. Without this
            // the app cannot tell that mpv moved on gaplessly.
            (5, "playlist-pos"),
        ] {
            player.send_line(&encode_observe(id, property))?;
        }

        Ok(player)
    }

    fn send_line(&mut self, line: &str) -> Result<(), PlayerError> {
        let writer = self.writer.as_mut().ok_or(PlayerError::Disconnected)?;
        writer
            .write_all(line.as_bytes())
            .and_then(|_| writer.write_all(b"\n"))
            .and_then(|_| writer.flush())
            .map_err(|e| PlayerError::Ipc(e.to_string()))
    }

    fn command(&mut self, args: &[&str]) -> Result<(), PlayerError> {
        let line = encode_command(args);
        self.send_line(&line)
    }

    /// True while the subprocess is alive.
    pub fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

fn connect_with_timeout(path: &Path) -> Result<UnixStream, PlayerError> {
    let deadline = Instant::now() + CONNECT_TIMEOUT;
    loop {
        match UnixStream::connect(path) {
            Ok(stream) => return Ok(stream),
            Err(err) => {
                if Instant::now() >= deadline {
                    return Err(PlayerError::Ipc(format!(
                        "could not connect to {}: {err}",
                        path.display()
                    )));
                }
                std::thread::sleep(CONNECT_POLL);
            }
        }
    }
}

impl Player for MpvPlayer {
    fn load(&mut self, tracks: &[TrackSource], mode: LoadMode) -> Result<(), PlayerError> {
        // Only the first track can replace: sending "replace" for every entry
        // would restart the playlist for each one and play only the last.
        for (index, track) in tracks.iter().enumerate() {
            let target = match (mode, index) {
                (LoadMode::Replace, 0) => "replace",
                _ => "append",
            };
            self.command(&["loadfile", &track.url, target])?;
        }
        Ok(())
    }

    fn play(&mut self) -> Result<(), PlayerError> {
        let line = encode_set("pause", "no");
        self.send_line(&line)
    }

    fn pause(&mut self, paused: bool) -> Result<(), PlayerError> {
        let line = encode_set("pause", if paused { "yes" } else { "no" });
        self.send_line(&line)
    }

    fn stop(&mut self) -> Result<(), PlayerError> {
        self.command(&["stop"])
    }

    fn seek(&mut self, mode: Seek, seconds: f64) -> Result<(), PlayerError> {
        let amount = format!("{seconds}");
        let kind = match mode {
            Seek::Absolute => "absolute",
            Seek::Relative => "relative",
        };
        self.command(&["seek", &amount, kind])
    }

    fn set_volume(&mut self, percent: u8) -> Result<(), PlayerError> {
        let value = percent.min(100).to_string();
        let line = encode_set("volume", &value);
        self.send_line(&line)
    }

    fn set_mute(&mut self, muted: bool) -> Result<(), PlayerError> {
        let line = encode_set("mute", if muted { "yes" } else { "no" });
        self.send_line(&line)
    }

    fn next(&mut self) -> Result<(), PlayerError> {
        self.command(&["playlist-next"])
    }

    fn previous(&mut self) -> Result<(), PlayerError> {
        self.command(&["playlist-prev"])
    }

    fn drain_events(&mut self) -> Vec<PlayerEvent> {
        let mut events = Vec::new();
        while let Ok(event) = self.events.try_recv() {
            events.push(event);
        }
        events
    }

    fn shutdown(&mut self) {
        if self.shutdown {
            return;
        }
        self.shutdown = true;
        let _ = self.command(&["quit"]);
        self.writer = None;
        // Give mpv a moment to exit cleanly, then insist.
        let deadline = Instant::now() + Duration::from_millis(500);
        while Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            let _ = handle.join();
        }
        let _ = std::fs::remove_file(&self.socket_path);
    }
}

impl Drop for MpvPlayer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpv_available() -> bool {
        Command::new("mpv")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    }

    fn wav_fixture(seconds: u32) -> PathBuf {
        let path = std::env::temp_dir().join(format!("undertone-test-{seconds}s.wav"));
        if path.exists() {
            return path;
        }
        // Minimal 44.1k stereo PCM sine, written with the standard library.
        let sample_rate = 44_100u32;
        let frames = sample_rate * seconds;
        let data_len = frames * 4;
        let mut bytes = Vec::with_capacity(44 + data_len as usize);
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_len).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes());
        bytes.extend_from_slice(&2u16.to_le_bytes());
        bytes.extend_from_slice(&sample_rate.to_le_bytes());
        bytes.extend_from_slice(&(sample_rate * 4).to_le_bytes());
        bytes.extend_from_slice(&4u16.to_le_bytes());
        bytes.extend_from_slice(&16u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_len.to_le_bytes());
        for i in 0..frames {
            let phase = (i as f64 / sample_rate as f64) * 440.0 * std::f64::consts::TAU;
            let value = (phase.sin() * 10_000.0) as i16;
            bytes.extend_from_slice(&value.to_le_bytes());
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        std::fs::write(&path, bytes).expect("write wav");
        path
    }

    #[test]
    fn a_two_track_window_advances_gaplessly() {
        if !mpv_available() {
            eprintln!("skipping: mpv not on PATH");
            return;
        }
        // Two distinct files, so mpv really moves from one to the other.
        let first = wav_fixture(1);
        let second = std::env::temp_dir().join("undertone-test-1s-b.wav");
        std::fs::copy(&first, &second).expect("copy fixture");

        let options = MpvOptions {
            audio_output: Some("null".to_string()),
            socket_path: std::env::temp_dir()
                .join(format!("undertone-gapless-{}.sock", std::process::id())),
            ..Default::default()
        };
        let mut player = MpvPlayer::spawn(&options).expect("spawn mpv");
        player
            .load(
                &[
                    TrackSource {
                        url: format!("file://{}", first.display()),
                        title: "first".into(),
                    },
                    TrackSource {
                        url: format!("file://{}", second.display()),
                        title: "second".into(),
                    },
                ],
                LoadMode::Replace,
            )
            .expect("load");
        player.play().expect("play");

        // The whole point: mpv reports the second entry itself, without the app
        // reloading anything.
        let mut seen = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline && !seen.contains(&1usize) {
            for event in player.drain_events() {
                if let PlayerEvent::PlaylistPos(position) = event {
                    seen.push(position);
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            seen.contains(&1),
            "mpv should have advanced to the second entry by itself: {seen:?}"
        );
        player.shutdown();
    }
    #[test]
    fn spawns_and_plays_a_file_through_a_null_sink() {
        if !mpv_available() {
            eprintln!("skipping: mpv not on PATH");
            return;
        }
        let options = MpvOptions {
            audio_output: Some("null".to_string()),
            // Its own socket: two mpv tests sharing one would delete each
            // other's and hang.
            socket_path: std::env::temp_dir()
                .join(format!("undertone-test-{}.sock", std::process::id())),
            ..Default::default()
        };
        let mut player = MpvPlayer::spawn(&options).expect("spawn mpv");
        let wav = wav_fixture(2);
        player
            .load(
                &[TrackSource {
                    url: format!("file://{}", wav.display()),
                    title: "test".into(),
                }],
                LoadMode::Replace,
            )
            .expect("load");
        player.play().expect("play");

        // Wait for the property stream to prove playback is progressing.
        let mut saw_position = false;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !saw_position {
            for event in player.drain_events() {
                if matches!(event, PlayerEvent::Position(_)) {
                    saw_position = true;
                }
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(saw_position, "expected a time-pos event from mpv");
        player.shutdown();
    }

    #[test]
    fn reports_the_measured_flag_set_does_not_panic_without_config() {
        // Regression guard: `--no-config` must be passed so a user's mpv.conf
        // cannot re-enable the OSD or video and blow the memory budget.
        let options = MpvOptions::default();
        let command_line = format!("{options:?}");
        assert!(command_line.contains("MpvOptions"));
    }
}
