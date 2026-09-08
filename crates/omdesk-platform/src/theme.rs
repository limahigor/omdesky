use serde::Deserialize;
use std::{fs, path::Path};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rgb {
    pub red: u8,
    pub green: u8,
    pub blue: u8,
}

impl Rgb {
    pub const fn new(red: u8, green: u8, blue: u8) -> Self {
        Self { red, green, blue }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmarchyTheme {
    pub accent: Rgb,
    pub selection: Rgb,
    pub muted: Rgb,
    pub background: Rgb,
    pub foreground: Rgb,
    pub error: Rgb,
    pub success: Rgb,
    pub warning: Rgb,
}

impl Default for OmarchyTheme {
    fn default() -> Self {
        Self {
            accent: Rgb::new(0x7a, 0xa2, 0xf7),
            selection: Rgb::new(0x33, 0x3b, 0x55),
            muted: Rgb::new(0x56, 0x5f, 0x89),
            background: Rgb::new(0x1a, 0x1b, 0x26),
            foreground: Rgb::new(0xc0, 0xca, 0xf5),
            error: Rgb::new(0xf7, 0x76, 0x8e),
            success: Rgb::new(0x9e, 0xce, 0x6a),
            warning: Rgb::new(0xe0, 0xaf, 0x68),
        }
    }
}

#[derive(Deserialize)]
struct RawTheme {
    accent: String,
    selection: String,
    muted: String,
    background: String,
    foreground: String,
    red: String,
    green: String,
    yellow: String,
}

pub struct ThemeWatcher {
    path: std::path::PathBuf,
    source: Option<String>,
    theme: OmarchyTheme,
}

impl ThemeWatcher {
    pub fn new(path: impl Into<std::path::PathBuf>) -> Self {
        let mut watcher = Self {
            path: path.into(),
            source: None,
            theme: OmarchyTheme::default(),
        };
        watcher.refresh();

        watcher
    }

    pub fn current(&self) -> &OmarchyTheme {
        &self.theme
    }

    pub fn refresh(&mut self) -> bool {
        let source = fs::read_to_string(&self.path).ok();

        if source == self.source {
            return false;
        }

        self.theme = source
            .as_deref()
            .and_then(|content| parse_theme(content).ok())
            .unwrap_or_default();
        self.source = source;

        true
    }
}

pub fn load_theme(path: &Path) -> OmarchyTheme {
    ThemeWatcher::new(path).theme
}

pub fn parse_theme(content: &str) -> Result<OmarchyTheme, ThemeError> {
    let raw: RawTheme = toml::from_str(content)?;

    Ok(OmarchyTheme {
        accent: parse_color("accent", &raw.accent)?,
        selection: parse_color("selection", &raw.selection)?,
        muted: parse_color("muted", &raw.muted)?,
        background: parse_color("background", &raw.background)?,
        foreground: parse_color("foreground", &raw.foreground)?,
        error: parse_color("red", &raw.red)?,
        success: parse_color("green", &raw.green)?,
        warning: parse_color("yellow", &raw.yellow)?,
    })
}

fn parse_color(name: &'static str, value: &str) -> Result<Rgb, ThemeError> {
    let hex = value
        .strip_prefix('#')
        .filter(|hex| hex.len() == 6)
        .ok_or(ThemeError::InvalidColor(name))?;
    let value = u32::from_str_radix(hex, 16).map_err(|_| ThemeError::InvalidColor(name))?;

    Ok(Rgb::new(
        ((value >> 16) & 0xff) as u8,
        ((value >> 8) & 0xff) as u8,
        (value & 0xff) as u8,
    ))
}

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("invalid Omarchy theme color: {0}")]
    InvalidColor(&'static str),
    #[error("invalid Omarchy theme file: {0}")]
    Toml(#[from] toml::de::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_theme_uses_omarchy_color_roles() {
        let theme = parse_theme(
            r##"
mode = "dark"
accent = "#7daea3"
selection = "#504945"
muted = "#665c54"
background = "#282828"
foreground = "#d4be98"
red = "#ea6962"
green = "#a9b665"
yellow = "#d8a657"
"##,
        )
        .expect("valid Omarchy theme");

        assert_eq!(theme.accent, Rgb::new(0x7d, 0xae, 0xa3));
        assert_eq!(theme.background, Rgb::new(0x28, 0x28, 0x28));
        assert_eq!(theme.foreground, Rgb::new(0xd4, 0xbe, 0x98));
    }

    #[test]
    fn test_load_theme_falls_back_when_current_theme_is_missing() {
        let theme = load_theme(Path::new("/path/that/does/not/exist"));

        assert_eq!(theme, OmarchyTheme::default());
    }

    #[test]
    fn test_theme_watcher_reloads_changed_theme() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("colors.toml");
        let theme = |accent: &str| {
            format!(
                r##"
accent = "{accent}"
selection = "#222222"
muted = "#333333"
background = "#000000"
foreground = "#ffffff"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
"##
            )
        };
        std::fs::write(&path, theme("#112233")).expect("write first theme");
        let mut watcher = ThemeWatcher::new(&path);

        std::fs::write(&path, theme("#445566")).expect("write second theme");

        assert!(watcher.refresh());
        assert_eq!(watcher.current().accent, Rgb::new(0x44, 0x55, 0x66));
    }

    #[test]
    fn test_parse_theme_rejects_invalid_hex_color() {
        let error = parse_theme(
            r##"
accent = "blue"
selection = "#222222"
muted = "#333333"
background = "#000000"
foreground = "#ffffff"
red = "#ff0000"
green = "#00ff00"
yellow = "#ffff00"
"##,
        )
        .expect_err("invalid color must fail");

        assert!(error.to_string().contains("accent"));
    }
}
