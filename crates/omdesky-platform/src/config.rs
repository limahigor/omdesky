use omdesky_core::{CodecPreference, DisplayMode, InputMode};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, env, fs, path::PathBuf};

pub const DEFAULT_AGENT_PORT: u16 = 48155;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub general: GeneralConfig,
    pub network: NetworkConfig,
    pub stream: StreamConfig,
    pub display: DisplayConfig,
    pub input: InputConfig,
    pub files: FilesConfig,
    #[serde(default)]
    pub devices: BTreeMap<String, DeviceConfig>,
}

impl Config {
    pub fn load() -> Result<Self, ConfigError> {
        let path = config_path()?;

        let mut config = if path.exists() {
            toml::from_str(&fs::read_to_string(path)?)?
        } else {
            Self::default()
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
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, toml::to_string_pretty(self)?)?;

        Ok(())
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GeneralConfig {
    pub notifications: bool,
    pub default_input: InputMode,
}

impl Default for GeneralConfig {
    fn default() -> Self {
        Self {
            notifications: true,
            default_input: InputMode::Remote,
        }
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct InputConfig {
    pub escape_chord: String,
}

impl Default for InputConfig {
    fn default() -> Self {
        Self {
            escape_chord: "CTRL+ALT+SHIFT+Z".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FilesConfig {
    pub enabled: bool,
    pub inbox: PathBuf,
}

impl Default for FilesConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            inbox: PathBuf::from("~/Downloads/Omdesky"),
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DeviceConfig {
    pub alias: Option<String>,
    pub default_display: Option<String>,
    pub default_workspace: Option<String>,
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

pub fn legacy_sunshine_credentials_path() -> Result<PathBuf, ConfigError> {
    config_dir().map(|directory| directory.join("sunshine-credentials.json"))
}

pub fn sunshine_config_path() -> Result<PathBuf, ConfigError> {
    Ok(home_dir()?.join(".config/sunshine/sunshine.conf"))
}

fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(path) = env::var_os("XDG_CONFIG_HOME") {
        return Ok(PathBuf::from(path).join("omdesky"));
    }

    Ok(home_dir()?.join(".config/omdesky"))
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
        assert_eq!(config.general.default_input, InputMode::Remote);
        assert_eq!(config.network.agent_port, 48155);
    }

    #[test]
    fn test_config_accepts_partial_toml() {
        let config: Config = toml::from_str("[stream]\nfps = 120\n").expect("valid config");

        assert_eq!(config.stream.fps, 120);
        assert_eq!(config.stream.width, 1920);
    }

    #[test]
    fn test_save_to_persists_stream_settings() {
        let path = env::temp_dir().join(format!("omdesky-config-{}.toml", std::process::id()));

        let mut config = Config::default();
        config.stream.bitrate_mbps = 25;
        config.stream.fps = 120;

        config.save_to(&path).expect("config saves");
        let saved: Config = toml::from_str(&fs::read_to_string(&path).expect("config readable"))
            .expect("saved config parses");
        fs::remove_file(path).expect("temporary config removed");

        assert_eq!(saved.stream.bitrate_mbps, 25);
        assert_eq!(saved.stream.fps, 120);
    }
}
