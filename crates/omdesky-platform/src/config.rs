use crate::state::{StateError, atomic_write_bytes, read_limited_to_string};
use omdesky_core::{CodecPreference, DisplayMode};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, env, path::PathBuf};

pub const DEFAULT_AGENT_PORT: u16 = 48155;
pub const MAX_CONFIG_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub network: NetworkConfig,
    pub stream: StreamConfig,
    pub display: DisplayConfig,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceConfig>,
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path()?;

        let mut config = match read_limited_to_string(&path, MAX_CONFIG_BYTES) {
            Ok(contents) => toml::from_str(&contents)?,
            Err(StateError::Io(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                Self::default()
            }
            Err(error) => return Err(ConfigError::State(error)),
        };

        if let Ok(port) = env::var("OMDESKY_AGENT_PORT") {
            config.network.agent_port = port
                .parse()
                .map_err(|_| ConfigError::InvalidEnvironment("OMDESKY_AGENT_PORT"))?;
        }

        Ok(config)
    }

    pub fn save(&self) -> Result<(), ConfigError> {
        self.save_to(&config_path()?)
    }

    pub fn save_to(&self, path: &std::path::Path) -> Result<(), ConfigError> {
        atomic_write_bytes(path, toml::to_string_pretty(self)?.as_bytes(), false)
            .map_err(ConfigError::State)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct NetworkConfig {
    pub agent_port: u16,
    pub strict_tailnet_only: bool,
    pub allow_unsafe_wildcard_bind: bool,
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            agent_port: DEFAULT_AGENT_PORT,
            strict_tailnet_only: true,
            allow_unsafe_wildcard_bind: false,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct StreamConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u16,
    pub codec: CodecPreference,
    pub audio: bool,
    pub bitrate_mbps: u32,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            codec: CodecPreference::Auto,
            audio: true,
            bitrate_mbps: 0,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DisplayConfig {
    pub mode: DisplayMode,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    pub alias: Option<String>,
}

pub fn config_path() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("omdesky/config.toml"));
    }

    Ok(home_dir()?.join(".config/omdesky/config.toml"))
}

pub fn state_dir() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("XDG_STATE_HOME") {
        return Ok(PathBuf::from(path).join("omdesky"));
    }

    Ok(home_dir()?.join(".local/state/omdesky"))
}

pub fn runtime_dir() -> Result<PathBuf, ConfigError> {
    env::var_os("XDG_RUNTIME_DIR")
        .map(PathBuf::from)
        .map(|path| path.join("omdesky"))
        .ok_or(ConfigError::MissingEnvironment("XDG_RUNTIME_DIR"))
}

pub fn runtime_session_path() -> Result<PathBuf, ConfigError> {
    runtime_dir().map(|directory| directory.join("current-session.json"))
}

pub fn access_path() -> Result<PathBuf, ConfigError> {
    state_dir().map(|directory| directory.join("access/allowlist.json"))
}

pub fn sunshine_config_path() -> Result<PathBuf, ConfigError> {
    Ok(home_dir()?.join(".config/sunshine/sunshine.conf"))
}

fn home_dir() -> Result<PathBuf, ConfigError> {
    env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or(ConfigError::MissingEnvironment("HOME"))
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("missing required environment variable {0}")]
    MissingEnvironment(&'static str),
    #[error("invalid environment variable {0}")]
    InvalidEnvironment(&'static str),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Toml(#[from] toml::de::Error),
    #[error(transparent)]
    TomlSerialize(#[from] toml::ser::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_uses_secure_defaults() {
        let config = Config::default();

        assert!(config.network.strict_tailnet_only);
        assert!(!config.network.allow_unsafe_wildcard_bind);
        assert_eq!(config.network.agent_port, 48155);
    }

    #[test]
    fn test_config_ignores_sections_from_earlier_releases() {
        let config: Config = toml::from_str(
            "[general]\nnotifications = true\n[input]\nescape_chord = \"CTRL+Z\"\n[files]\nenabled = false\n[stream]\nfps = 90\n",
        )
        .expect("earlier configuration still parses");

        assert_eq!(config.stream.fps, 90);
    }

    #[test]
    fn test_config_accepts_partial_toml() {
        let config: Config = toml::from_str("[stream]\nfps = 120\n").expect("valid config");

        assert_eq!(config.stream.fps, 120);
        assert_eq!(config.stream.width, 1920);
    }

    #[test]
    fn test_save_to_persists_stream_settings() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("nested/config.toml");

        let mut config = Config::default();
        config.stream.bitrate_mbps = 25;
        config.stream.fps = 120;

        config.save_to(&path).expect("config saves");
        let saved: Config = toml::from_str(
            &read_limited_to_string(&path, MAX_CONFIG_BYTES).expect("config readable"),
        )
        .expect("saved config parses");

        assert_eq!(saved.stream.bitrate_mbps, 25);
        assert_eq!(saved.stream.fps, 120);
    }

    #[test]
    fn test_save_to_replaces_an_existing_file_without_following_a_symlink() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let target = directory.path().join("outside.toml");
        let path = directory.path().join("config.toml");
        std::fs::write(&target, "poisoned").expect("target written");

        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &path).expect("symlink created");

        Config::default().save_to(&path).expect("config saves");

        assert_eq!(
            std::fs::read_to_string(&target).expect("target readable"),
            "poisoned"
        );
        assert!(
            !std::fs::symlink_metadata(&path)
                .expect("metadata")
                .file_type()
                .is_symlink()
        );
    }
}
