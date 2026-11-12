//! Configuration: file discovery, password resolution, derived paths.
//!
//! Precedence is command line → environment → user config → system config.
//! Passwords never go in a world-readable file: prefer `password_cmd`.

use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("no server configured (set `server` in the config or pass --server)")]
    MissingServer,
    #[error("no username configured (set `username` in the config)")]
    MissingUsername,
    #[error("no password: set `password_cmd` or `password`, or UNDERTONE_PASSWORD")]
    MissingPassword,
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid config {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("password_cmd failed: {0}")]
    PasswordCmd(String),
}

#[derive(Debug, Clone, Default, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub server: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    /// Command whose stdout is the password, e.g. `["pass", "show", "navidrome"]`.
    pub password_cmd: Option<Vec<String>>,
    pub scrobble: Option<bool>,
    pub auto_dj: Option<bool>,
    /// Default album list sort: random, newest, alphabeticalByArtist, ...
    pub start_view: Option<String>,
    #[serde(default)]
    pub art: ArtConfig,
    #[serde(default)]
    pub player: PlayerConfig,
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtConfig {
    /// auto | sixel | kitty | symbols | off
    #[serde(default = "default_art_mode")]
    pub mode: String,
    #[serde(default = "default_thumb_colors")]
    pub thumb_colors: u32,
    #[serde(default = "default_cover_colors")]
    pub cover_colors: u32,
    #[serde(default = "default_cache_mb")]
    pub cache_mb: u64,
    /// Colour painted over a cover that is being removed, as `#rrggbb`.
    ///
    /// sixel has no delete operation, so a rectangle is cleared by drawing a
    /// solid image over it. Set this to the terminal's background colour.
    #[serde(default = "default_backdrop")]
    pub backdrop: String,
}

impl Default for ArtConfig {
    fn default() -> Self {
        Self {
            mode: default_art_mode(),
            thumb_colors: default_thumb_colors(),
            cover_colors: default_cover_colors(),
            cache_mb: default_cache_mb(),
            backdrop: default_backdrop(),
        }
    }
}

#[derive(Debug, Clone, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlayerConfig {
    #[serde(default = "default_mpv")]
    pub mpv: String,
    /// never | mp3-192
    #[serde(default = "default_transcode")]
    pub transcode: String,
}

impl Default for PlayerConfig {
    fn default() -> Self {
        Self {
            mpv: default_mpv(),
            transcode: default_transcode(),
        }
    }
}

fn default_art_mode() -> String {
    "auto".to_string()
}

fn default_backdrop() -> String {
    "#000000".to_string()
}

/// Parse `#rrggbb` (or `rrggbb`). Anything else falls back to black.
pub fn parse_backdrop(value: &str) -> [u8; 3] {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() != 6 {
        return [0, 0, 0];
    }
    let channel = |range: std::ops::Range<usize>| u8::from_str_radix(&hex[range], 16).unwrap_or(0);
    [channel(0..2), channel(2..4), channel(4..6)]
}

fn default_thumb_colors() -> u32 {
    16
}
fn default_cover_colors() -> u32 {
    240
}
fn default_cache_mb() -> u64 {
    20
}
fn default_mpv() -> String {
    "mpv".to_string()
}
fn default_transcode() -> String {
    "never".to_string()
}

/// Fully resolved runtime settings.
#[derive(Debug, Clone)]
pub struct Settings {
    pub server: String,
    pub username: String,
    pub password: String,
    pub scrobble: bool,
    pub auto_dj: bool,
    pub art_mode: String,
    pub thumb_colors: u32,
    pub cover_colors: u32,
    pub cache_dir: PathBuf,
    pub cache_bytes: u64,
    pub mpv_binary: PathBuf,
    pub transcode: bool,
    /// Colour used to overpaint a removed cover under sixel.
    pub backdrop: [u8; 3],
}

impl Config {
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        toml::from_str(text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    /// Load the first config file that exists, or defaults.
    pub fn load(
        explicit: Option<&Path>,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(Self, Option<PathBuf>), ConfigError> {
        let mut candidates = Vec::new();
        if let Some(path) = explicit {
            candidates.push(path.to_path_buf());
        } else {
            candidates.push(user_config_path(env));
            candidates.push(PathBuf::from("/etc/undertone/config.toml"));
        }

        for path in candidates {
            match std::fs::read_to_string(&path) {
                Ok(text) => {
                    let config = Self::parse(&text, &path)?;
                    return Ok((config, Some(path)));
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => continue,
                Err(source) => return Err(ConfigError::Read { path, source }),
            }
        }
        Ok((Self::default(), None))
    }

    /// Merge command-line overrides and resolve the password.
    pub fn resolve(
        self,
        cli: CliOverrides,
        env: &dyn Fn(&str) -> Option<String>,
    ) -> Result<Settings, ConfigError> {
        let server = cli
            .server
            .or_else(|| env("UNDERTONE_SERVER"))
            .or(self.server)
            .ok_or(ConfigError::MissingServer)?;
        let username = cli
            .username
            .or_else(|| env("UNDERTONE_USERNAME"))
            .or(self.username)
            .ok_or(ConfigError::MissingUsername)?;

        let password = match self.password_cmd.as_ref() {
            Some(argv) if !argv.is_empty() => run_password_cmd(argv)?,
            _ => env("UNDERTONE_PASSWORD")
                .or(self.password)
                .ok_or(ConfigError::MissingPassword)?,
        };

        let home = env("HOME").unwrap_or_else(|| "/".to_string());
        let cache_dir = match env("XDG_CACHE_HOME") {
            Some(base) if !base.is_empty() => PathBuf::from(base).join("undertone"),
            _ => PathBuf::from(&home).join(".cache").join("undertone"),
        };

        Ok(Settings {
            server,
            username,
            password,
            scrobble: self.scrobble.unwrap_or(true),
            auto_dj: self.auto_dj.unwrap_or(true),
            art_mode: cli.art.unwrap_or(self.art.mode),
            thumb_colors: self.art.thumb_colors,
            cover_colors: self.art.cover_colors,
            cache_dir,
            cache_bytes: self.art.cache_mb.saturating_mul(1024 * 1024),
            mpv_binary: PathBuf::from(self.player.mpv),
            transcode: matches!(self.player.transcode.as_str(), "mp3" | "mp3-192"),
            backdrop: parse_backdrop(&self.art.backdrop),
        })
    }
}

/// Command-line values that override the file.
#[derive(Debug, Clone, Default)]
pub struct CliOverrides {
    pub server: Option<String>,
    pub username: Option<String>,
    pub art: Option<String>,
}

fn user_config_path(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    match env("XDG_CONFIG_HOME") {
        Some(base) if !base.is_empty() => PathBuf::from(base).join("undertone/config.toml"),
        _ => {
            let home = env("HOME").unwrap_or_else(|| "/".to_string());
            PathBuf::from(home).join(".config/undertone/config.toml")
        }
    }
}

fn run_password_cmd(argv: &[String]) -> Result<String, ConfigError> {
    let output = Command::new(&argv[0])
        .args(&argv[1..])
        .output()
        .map_err(|err| ConfigError::PasswordCmd(format!("{}: {err}", argv[0])))?;
    if !output.status.success() {
        return Err(ConfigError::PasswordCmd(format!(
            "{} exited with {}",
            argv[0], output.status
        )));
    }
    let password = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if password.is_empty() {
        return Err(ConfigError::PasswordCmd(format!(
            "{} produced no output",
            argv[0]
        )));
    }
    Ok(password)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_env(_: &str) -> Option<String> {
        None
    }

    fn env_of(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<String> {
        move |key| {
            pairs
                .iter()
                .find(|(k, _)| *k == key)
                .map(|(_, v)| (*v).to_string())
        }
    }

    #[test]
    fn a_full_config_parses() {
        let text = r#"
server = "https://music.example.org"
username = "david"
password = "hunter2"
scrobble = false
auto_dj = false

[art]
mode = "sixel"
thumb_colors = 16
cover_colors = 240
cache_mb = 32

[player]
mpv = "/run/current-system/sw/bin/mpv"
transcode = "mp3-192"
"#;
        let config = Config::parse(text, Path::new("test.toml")).unwrap();
        assert_eq!(config.server.as_deref(), Some("https://music.example.org"));
        assert_eq!(config.scrobble, Some(false));
        assert_eq!(config.art.thumb_colors, 16);
        assert_eq!(config.art.cache_mb, 32);
        assert!(config.player.transcode.contains("192"));
    }

    #[test]
    fn a_minimal_config_gets_sensible_defaults() {
        let config = Config::parse("server = \"https://x\"\n", Path::new("t")).unwrap();
        assert_eq!(config.art.mode, "auto");
        assert_eq!(config.art.thumb_colors, 16);
        assert_eq!(config.art.cover_colors, 240);
        assert_eq!(config.art.cache_mb, 20);
        assert_eq!(config.player.mpv, "mpv");
        assert_eq!(config.player.transcode, "never");
    }

    #[test]
    fn unknown_keys_are_rejected() {
        // A typo in a config file should be loud, not silently ignored.
        let err = Config::parse("server = \"x\"\nservre = \"y\"\n", Path::new("t"));
        assert!(err.is_err());
    }

    #[test]
    fn resolve_requires_server_username_and_password() {
        let config = Config::parse("", Path::new("t")).unwrap();
        assert!(matches!(
            config.resolve(CliOverrides::default(), &no_env),
            Err(ConfigError::MissingServer)
        ));

        let config = Config::parse("server = \"https://x\"\n", Path::new("t")).unwrap();
        assert!(matches!(
            config.resolve(CliOverrides::default(), &no_env),
            Err(ConfigError::MissingUsername)
        ));

        let config =
            Config::parse("server = \"https://x\"\nusername = \"u\"\n", Path::new("t")).unwrap();
        assert!(matches!(
            config.resolve(CliOverrides::default(), &no_env),
            Err(ConfigError::MissingPassword)
        ));
    }

    #[test]
    fn cli_overrides_beat_the_file() {
        let config = Config::parse(
            "server = \"https://file\"\nusername = \"file-user\"\npassword = \"p\"\n",
            Path::new("t"),
        )
        .unwrap();
        let settings = config
            .resolve(
                CliOverrides {
                    server: Some("https://cli".into()),
                    username: None,
                    art: Some("symbols".into()),
                },
                &no_env,
            )
            .unwrap();
        assert_eq!(settings.server, "https://cli");
        assert_eq!(settings.username, "file-user");
        assert_eq!(settings.art_mode, "symbols");
    }

    #[test]
    fn environment_beats_the_file_but_loses_to_the_cli() {
        let config = Config::parse(
            "server = \"https://file\"\nusername = \"file-user\"\npassword = \"file-pass\"\n",
            Path::new("t"),
        )
        .unwrap();
        let env = env_of(&[("UNDERTONE_SERVER", "https://env")]);
        let settings = config
            .clone()
            .resolve(CliOverrides::default(), &env)
            .unwrap();
        assert_eq!(settings.server, "https://env");
        assert_eq!(settings.password, "file-pass");

        let settings = config
            .resolve(
                CliOverrides {
                    server: Some("https://cli".into()),
                    ..Default::default()
                },
                &env,
            )
            .unwrap();
        assert_eq!(settings.server, "https://cli");
    }

    #[test]
    fn password_cmd_wins_over_a_plain_password() {
        let config = Config::parse(
            "server = \"https://x\"\nusername = \"u\"\npassword = \"plain\"\npassword_cmd = [\"printf\", \"from-cmd\"]\n",
            Path::new("t"),
        )
        .unwrap();
        let settings = config.resolve(CliOverrides::default(), &no_env).unwrap();
        assert_eq!(settings.password, "from-cmd");
    }

    #[test]
    fn a_failing_password_cmd_is_reported() {
        let config = Config::parse(
            "server = \"https://x\"\nusername = \"u\"\npassword_cmd = [\"false\"]\n",
            Path::new("t"),
        )
        .unwrap();
        assert!(matches!(
            config.resolve(CliOverrides::default(), &no_env),
            Err(ConfigError::PasswordCmd(_))
        ));
    }

    #[test]
    fn cache_dir_honours_xdg() {
        let config = Config::parse(
            "server = \"https://x\"\nusername = \"u\"\npassword = \"p\"\n",
            Path::new("t"),
        )
        .unwrap();
        let env = env_of(&[("XDG_CACHE_HOME", "/tmp/xdg-cache")]);
        let settings = config.resolve(CliOverrides::default(), &env).unwrap();
        assert_eq!(
            settings.cache_dir,
            PathBuf::from("/tmp/xdg-cache/undertone")
        );
    }

    #[test]
    fn user_config_path_follows_xdg() {
        let env = env_of(&[("XDG_CONFIG_HOME", "/tmp/cfg")]);
        assert_eq!(
            user_config_path(&env),
            PathBuf::from("/tmp/cfg/undertone/config.toml")
        );
        let env = env_of(&[("HOME", "/home/david")]);
        assert_eq!(
            user_config_path(&env),
            PathBuf::from("/home/david/.config/undertone/config.toml")
        );
    }

    #[test]
    fn missing_config_files_are_not_an_error() {
        let (config, path) = Config::load(Some(Path::new("/nonexistent/undertone.toml")), &no_env)
            .expect("a missing explicit file falls through to defaults");
        assert!(path.is_none());
        assert!(config.server.is_none());
    }
}
