#![forbid(unsafe_code)]

pub mod text;

use serde::{Deserialize, Serialize};
use std::{fmt, net::IpAddr, str::FromStr};
use thiserror::Error;
use uuid::Uuid;

macro_rules! uuid_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(value).map(Self)
            }
        }
    };
}

uuid_id!(NodeId);
uuid_id!(SessionId);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Ready,
    Blocked,
    Offline,
    Unavailable,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerCode {
    Incompatible,
    Denied,
    NeedsAccess,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockerSide {
    Local,
    Remote,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeBlocker {
    pub code: BlockerCode,
    pub side: BlockerSide,
    pub fix: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeCapabilities(Vec<String>);

impl NodeCapabilities {
    pub fn new(capabilities: impl IntoIterator<Item = String>) -> Self {
        let mut capabilities: Vec<_> = capabilities.into_iter().collect();
        capabilities.sort();
        capabilities.dedup();

        Self(capabilities)
    }

    pub fn contains(&self, capability: &str) -> bool {
        self.0.iter().any(|candidate| candidate == capability)
    }

    pub fn as_slice(&self) -> &[String] {
        &self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionKind {
    Direct,
    Relay,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MeshPeer {
    pub tailnet_node_id: String,
    pub dns_name: Option<String>,
    pub hostname: Option<String>,
    pub ips: Vec<IpAddr>,
    pub online: bool,
    pub connection: ConnectionKind,
    pub latency_ms: Option<u32>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Display {
    pub id: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub refresh_hz: f64,
    pub focused: bool,
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DisplayId(pub String);

impl DisplayId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        let value = self.as_str();
        if value.is_empty() || value.len() > 64 {
            return Err(DomainError::InvalidDisplayId);
        }

        if value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        {
            Ok(())
        } else {
            Err(DomainError::InvalidDisplayId)
        }
    }
}

impl fmt::Display for DisplayId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl From<&str> for DisplayId {
    fn from(value: &str) -> Self {
        Self(value.to_owned())
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DisplayMode {
    #[default]
    FollowFocus,
}

impl DisplayMode {
    pub const ALL: [Self; 1] = [Self::FollowFocus];

    pub fn label(self) -> &'static str {
        match self {
            DisplayMode::FollowFocus => "Follow Focus",
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct RemoteDesktopTopology {
    displays: Vec<DisplayId>,
    focused: Option<DisplayId>,
}

impl RemoteDesktopTopology {
    pub fn new(displays: impl IntoIterator<Item = DisplayId>, focused: Option<DisplayId>) -> Self {
        Self {
            displays: displays.into_iter().collect(),
            focused,
        }
    }

    pub fn from_displays(displays: &[Display]) -> Self {
        let focused = displays
            .iter()
            .find(|display| display.focused)
            .map(|display| DisplayId::new(display.id.clone()));

        Self {
            displays: displays
                .iter()
                .map(|display| DisplayId::new(display.id.clone()))
                .collect(),
            focused,
        }
    }

    pub fn displays(&self) -> &[DisplayId] {
        &self.displays
    }

    pub fn focused(&self) -> Option<&DisplayId> {
        self.focused.as_ref()
    }

    pub fn contains(&self, display: &DisplayId) -> bool {
        self.displays.contains(display)
    }

    pub fn preferred(&self) -> Option<&DisplayId> {
        self.focused.as_ref().or_else(|| self.displays.first())
    }
}

pub fn follow_focus_target(
    topology: &RemoteDesktopTopology,
    streamed: &DisplayId,
) -> Option<DisplayId> {
    let target = topology.preferred()?;
    (target != streamed).then(|| target.clone())
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WorkspaceId(pub i64);

impl fmt::Display for WorkspaceId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl FromStr for WorkspaceId {
    type Err = std::num::ParseIntError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        value.parse().map(WorkspaceId)
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WindowId(pub String);

impl fmt::Display for WindowId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: WorkspaceId,
    pub name: Option<String>,
    pub monitor: Option<String>,
    pub focused: bool,
    pub windows: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Window {
    pub id: WindowId,
    pub app_id: Option<String>,
    pub class: Option<String>,
    pub title: Option<String>,
    pub workspace: WorkspaceId,
    pub focused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceTarget {
    Id(WorkspaceId),
    Name(String),
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyModifier {
    Super,
    Ctrl,
    Alt,
    Shift,
}

impl KeyModifier {
    pub fn as_hypr(self) -> &'static str {
        match self {
            KeyModifier::Super => "SUPER",
            KeyModifier::Ctrl => "CTRL",
            KeyModifier::Alt => "ALT",
            KeyModifier::Shift => "SHIFT",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct KeyChord {
    mods: Vec<KeyModifier>,
    key: String,
}

impl KeyChord {
    pub fn new(
        mods: impl IntoIterator<Item = KeyModifier>,
        key: impl Into<String>,
    ) -> Result<Self, DomainError> {
        let mut mods: Vec<_> = mods.into_iter().collect();
        mods.sort();
        mods.dedup();

        let chord = Self {
            mods,
            key: key.into(),
        };
        chord.validate()?;

        Ok(chord)
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        if is_safe_key_token(&self.key) {
            Ok(())
        } else {
            Err(DomainError::InvalidKeyChord)
        }
    }

    pub fn modifiers(&self) -> &[KeyModifier] {
        &self.mods
    }

    pub fn key(&self) -> &str {
        &self.key
    }

    pub fn modifiers_hypr(&self) -> String {
        self.mods
            .iter()
            .map(|modifier| modifier.as_hypr())
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowSelector {
    ActiveWindow,
    Class(String),
    Address(String),
}

impl WindowSelector {
    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            WindowSelector::ActiveWindow => Ok(()),
            WindowSelector::Class(class) => {
                if !class.is_empty() && class.chars().all(is_safe_class_char) {
                    Ok(())
                } else {
                    Err(DomainError::InvalidWindowSelector)
                }
            }
            WindowSelector::Address(address) => {
                if is_window_address(address) {
                    Ok(())
                } else {
                    Err(DomainError::InvalidWindowSelector)
                }
            }
        }
    }

    pub fn as_hypr(&self) -> String {
        match self {
            WindowSelector::ActiveWindow => "activewindow".to_owned(),
            WindowSelector::Class(class) => format!("class:{class}"),
            WindowSelector::Address(address) => format!("address:{address}"),
        }
    }
}

pub fn is_tailscale_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(address) => {
            let octets = address.octets();

            octets[0] == 100 && (64..128).contains(&octets[1])
        }
        IpAddr::V6(address) => {
            let segments = address.segments();

            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

pub fn is_window_address(value: &str) -> bool {
    value.len() <= 34
        && value.strip_prefix("0x").is_some_and(|hex| {
            !hex.is_empty() && hex.chars().all(|character| character.is_ascii_hexdigit())
        })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRole {
    Controller,
    Remote,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionEndpoint {
    pub address: IpAddr,
    pub port: u16,
}

impl SessionEndpoint {
    pub fn validate(&self) -> Result<(), DomainError> {
        if self.port == 0 {
            Err(DomainError::InvalidSessionEndpoint)
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionClaim {
    pub id: SessionId,
    pub generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionGrant {
    pub id: SessionId,
    pub generation: u64,
    pub lease_seconds: u64,
}

impl SessionGrant {
    pub fn claim(&self) -> SessionClaim {
        SessionClaim {
            id: self.id,
            generation: self.generation,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ControlCapability {
    ReadMetadata,
    FocusWorkspace,
    ControlSession,
    SendShortcut,
    CloseStream,
    ApprovePairing,
}

impl ControlCapability {
    pub const ALL: [Self; 6] = [
        Self::ReadMetadata,
        Self::FocusWorkspace,
        Self::ControlSession,
        Self::SendShortcut,
        Self::CloseStream,
        Self::ApprovePairing,
    ];

    pub const CALLBACK: [Self; 2] = [Self::SendShortcut, Self::CloseStream];

    pub fn as_str(self) -> &'static str {
        match self {
            ControlCapability::ReadMetadata => "read_metadata",
            ControlCapability::FocusWorkspace => "focus_workspace",
            ControlCapability::ControlSession => "control_session",
            ControlCapability::SendShortcut => "send_shortcut",
            ControlCapability::CloseStream => "close_stream",
            ControlCapability::ApprovePairing => "approve_pairing",
        }
    }
}

impl fmt::Display for ControlCapability {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for ControlCapability {
    type Err = DomainError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        ControlCapability::ALL
            .into_iter()
            .find(|capability| capability.as_str() == value)
            .ok_or(DomainError::UnknownCapability)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RemoteCommand {
    SendShortcut {
        chord: KeyChord,
        window: WindowSelector,
    },
    CloseWindow {
        window: WindowSelector,
    },
    SwitchStreamDisplay {
        display: DisplayId,
    },
    AttachSession {
        role: SessionRole,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<SessionEndpoint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        window: Option<WindowSelector>,
    },
    DetachSession,
    RenewSession,
}

impl RemoteCommand {
    pub fn kind(&self) -> &'static str {
        match self {
            RemoteCommand::SendShortcut { .. } => "send_shortcut",
            RemoteCommand::CloseWindow { .. } => "close_window",
            RemoteCommand::SwitchStreamDisplay { .. } => "switch_stream_display",
            RemoteCommand::AttachSession { .. } => "attach_session",
            RemoteCommand::DetachSession => "detach_session",
            RemoteCommand::RenewSession => "renew_session",
        }
    }

    pub fn validate(&self) -> Result<(), DomainError> {
        match self {
            RemoteCommand::SendShortcut { chord, window } => {
                chord.validate()?;
                window.validate()
            }
            RemoteCommand::CloseWindow { window } => window.validate(),
            RemoteCommand::SwitchStreamDisplay { display } => display.validate(),
            RemoteCommand::AttachSession {
                role,
                controller,
                window,
            } => {
                if let Some(window) = window {
                    window.validate()?;
                }

                match role {
                    SessionRole::Remote => controller
                        .as_ref()
                        .ok_or(DomainError::InvalidSessionEndpoint)
                        .and_then(SessionEndpoint::validate),
                    SessionRole::Controller => Ok(()),
                }
            }
            RemoteCommand::DetachSession | RemoteCommand::RenewSession => Ok(()),
        }
    }

    pub fn is_session_control(&self) -> bool {
        matches!(
            self,
            RemoteCommand::AttachSession { .. }
                | RemoteCommand::DetachSession
                | RemoteCommand::RenewSession
        )
    }

    pub fn required_capability(&self) -> ControlCapability {
        match self {
            RemoteCommand::SendShortcut { .. } | RemoteCommand::SwitchStreamDisplay { .. } => {
                ControlCapability::SendShortcut
            }
            RemoteCommand::CloseWindow { .. } => ControlCapability::CloseStream,
            RemoteCommand::AttachSession { .. }
            | RemoteCommand::DetachSession
            | RemoteCommand::RenewSession => ControlCapability::ControlSession,
        }
    }
}

fn is_safe_key_token(key: &str) -> bool {
    !key.is_empty() && key.len() <= 32 && key.chars().all(|c| c.is_ascii_alphanumeric())
}

fn is_safe_class_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodecPreference {
    Auto,
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct StreamProfile {
    pub width: u32,
    pub height: u32,
    pub fps: u16,
    pub codec_preference: CodecPreference,
    pub audio: bool,

    pub bitrate_kbps: Option<u32>,
}

pub const MIN_STREAM_WIDTH: u32 = 320;
pub const MAX_STREAM_WIDTH: u32 = 7680;
pub const MIN_STREAM_HEIGHT: u32 = 240;
pub const MAX_STREAM_HEIGHT: u32 = 4320;
pub const MIN_STREAM_FPS: u16 = 15;
pub const MAX_STREAM_FPS: u16 = 480;
pub const MIN_STREAM_BITRATE_KBPS: u32 = 500;
pub const MAX_STREAM_BITRATE_KBPS: u32 = 500_000;

impl StreamProfile {
    pub fn validate(&self) -> Result<(), DomainError> {
        let within_range = (MIN_STREAM_WIDTH..=MAX_STREAM_WIDTH).contains(&self.width)
            && (MIN_STREAM_HEIGHT..=MAX_STREAM_HEIGHT).contains(&self.height)
            && (MIN_STREAM_FPS..=MAX_STREAM_FPS).contains(&self.fps)
            && self.bitrate_kbps.is_none_or(|kbps| {
                (MIN_STREAM_BITRATE_KBPS..=MAX_STREAM_BITRATE_KBPS).contains(&kbps)
            });

        if within_range {
            Ok(())
        } else {
            Err(DomainError::InvalidStreamProfile)
        }
    }
}

pub fn bitrate_kbps_from_mbps(mbps: u32) -> Result<u32, DomainError> {
    mbps.checked_mul(1000)
        .filter(|kbps| (MIN_STREAM_BITRATE_KBPS..=MAX_STREAM_BITRATE_KBPS).contains(kbps))
        .ok_or(DomainError::InvalidStreamProfile)
}

impl Default for StreamProfile {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            codec_preference: CodecPreference::Auto,
            audio: true,
            bitrate_kbps: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputMode {
    Local,
    Remote,
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DomainError {
    #[error("key chord key must be a short ASCII-alphanumeric token")]
    InvalidKeyChord,
    #[error("window selector is not a safe class token")]
    InvalidWindowSelector,
    #[error("session endpoint requires a controller address and non-zero port")]
    InvalidSessionEndpoint,
    #[error("display identifier is not a safe monitor token")]
    InvalidDisplayId,
    #[error("stream profile is outside the supported resolution, frame rate or bitrate range")]
    InvalidStreamProfile,
    #[error("unknown control capability")]
    UnknownCapability,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capabilities_preserve_unknown_values() {
        let capabilities = NodeCapabilities::new([
            "desktop.stream-host".to_owned(),
            "future.capability".to_owned(),
        ]);

        assert!(capabilities.contains("future.capability"));
    }

    #[test]
    fn test_key_chord_rejects_unsafe_key_token() {
        assert_eq!(
            KeyChord::new([KeyModifier::Ctrl], "Z; rm"),
            Err(DomainError::InvalidKeyChord)
        );
        assert_eq!(
            KeyChord::new([KeyModifier::Ctrl], ""),
            Err(DomainError::InvalidKeyChord)
        );
    }

    #[test]
    fn test_key_chord_renders_sorted_deduplicated_modifiers() {
        let chord = KeyChord::new(
            [
                KeyModifier::Shift,
                KeyModifier::Ctrl,
                KeyModifier::Alt,
                KeyModifier::Ctrl,
            ],
            "Z",
        )
        .expect("valid chord");

        assert_eq!(chord.modifiers_hypr(), "CTRL ALT SHIFT");
        assert_eq!(chord.key(), "Z");
    }

    #[test]
    fn test_window_selector_accepts_reverse_dns_class() {
        let selector = WindowSelector::Class("com.moonlight_stream.Moonlight".to_owned());

        assert!(selector.validate().is_ok());
        assert_eq!(selector.as_hypr(), "class:com.moonlight_stream.Moonlight");
    }

    #[test]
    fn test_window_selector_rejects_unsafe_address() {
        assert_eq!(
            WindowSelector::Address("0x55; rm -rf /".to_owned()).validate(),
            Err(DomainError::InvalidWindowSelector)
        );
        assert_eq!(
            WindowSelector::Address("moonlight".to_owned()).validate(),
            Err(DomainError::InvalidWindowSelector)
        );
        assert_eq!(
            WindowSelector::Address(format!("0x{}", "a".repeat(64))).validate(),
            Err(DomainError::InvalidWindowSelector)
        );
    }

    #[test]
    fn test_only_the_cgnat_range_counts_as_tailscale() {
        assert!(is_tailscale_address(
            "100.64.0.1".parse::<IpAddr>().expect("address")
        ));
        assert!(is_tailscale_address(
            "100.127.255.254".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "100.0.0.1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "100.63.255.255".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "100.128.0.1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "192.168.1.10".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "127.0.0.1".parse::<IpAddr>().expect("address")
        ));
    }

    #[test]
    fn test_only_the_tailscale_ula_prefix_counts_as_tailscale() {
        assert!(is_tailscale_address(
            "fd7a:115c:a1e0::1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "fd7a:0000:0000::1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "fd7a:115c:a1e1::1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "::1".parse::<IpAddr>().expect("address")
        ));
    }

    #[test]
    fn test_capabilities_round_trip_through_their_wire_names() {
        for capability in ControlCapability::ALL {
            assert_eq!(
                capability.as_str().parse::<ControlCapability>(),
                Ok(capability)
            );
        }

        assert_eq!(
            "root_shell".parse::<ControlCapability>(),
            Err(DomainError::UnknownCapability)
        );
    }

    #[test]
    fn test_commands_require_their_narrowest_capability() {
        assert_eq!(
            RemoteCommand::DetachSession.required_capability(),
            ControlCapability::ControlSession
        );
        assert_eq!(
            RemoteCommand::CloseWindow {
                window: WindowSelector::ActiveWindow
            }
            .required_capability(),
            ControlCapability::CloseStream
        );
        assert_eq!(
            RemoteCommand::SwitchStreamDisplay {
                display: DisplayId::from("DP-2")
            }
            .required_capability(),
            ControlCapability::SendShortcut
        );
    }

    #[test]
    fn test_renew_session_is_session_control() {
        let command: RemoteCommand =
            serde_json::from_str(r#"{"action":"renew_session"}"#).expect("valid renew_session");

        assert!(command.is_session_control());
        assert_eq!(command.kind(), "renew_session");
        assert!(command.validate().is_ok());
    }

    #[test]
    fn test_session_grant_yields_a_matching_claim() {
        let grant = SessionGrant {
            id: SessionId::new(),
            generation: 7,
            lease_seconds: 30,
        };

        assert_eq!(
            grant.claim(),
            SessionClaim {
                id: grant.id,
                generation: 7
            }
        );
    }

    #[test]
    fn test_stream_profile_rejects_out_of_range_geometry() {
        let mut profile = StreamProfile::default();
        assert!(profile.validate().is_ok());

        profile.width = 0;
        assert_eq!(profile.validate(), Err(DomainError::InvalidStreamProfile));

        let mut profile = StreamProfile {
            fps: 1_000,
            ..StreamProfile::default()
        };
        assert_eq!(profile.validate(), Err(DomainError::InvalidStreamProfile));

        profile.fps = 60;
        profile.bitrate_kbps = Some(u32::MAX);
        assert_eq!(profile.validate(), Err(DomainError::InvalidStreamProfile));
    }

    #[test]
    fn test_bitrate_conversion_rejects_overflow_and_out_of_range_values() {
        assert_eq!(bitrate_kbps_from_mbps(25), Ok(25_000));
        assert_eq!(
            bitrate_kbps_from_mbps(u32::MAX),
            Err(DomainError::InvalidStreamProfile)
        );
        assert_eq!(
            bitrate_kbps_from_mbps(4_300_000),
            Err(DomainError::InvalidStreamProfile)
        );
        assert_eq!(
            bitrate_kbps_from_mbps(0),
            Err(DomainError::InvalidStreamProfile)
        );
    }

    #[test]
    fn test_window_selector_rejects_unsafe_class() {
        assert_eq!(
            WindowSelector::Class("class:foo bar".to_owned()).validate(),
            Err(DomainError::InvalidWindowSelector)
        );
    }

    #[test]
    fn test_remote_command_validate_rejects_deserialized_unsafe_payload() {
        let command: RemoteCommand = serde_json::from_str(
            r#"{"action":"send_shortcut","chord":{"mods":["ctrl"],"key":"Z Z"},"window":"active_window"}"#,
        )
        .expect("known action deserializes");

        assert_eq!(command.validate(), Err(DomainError::InvalidKeyChord));
    }

    #[test]
    fn test_remote_command_rejects_unknown_action_tag() {
        let error = serde_json::from_str::<RemoteCommand>(
            r#"{"action":"format_disk","target":"/dev/sda"}"#,
        );

        assert!(error.is_err());
    }

    #[test]
    fn test_attach_session_requires_controller_for_remote_role() {
        let remote_without_controller = RemoteCommand::AttachSession {
            role: SessionRole::Remote,
            controller: None,
            window: None,
        };
        assert_eq!(
            remote_without_controller.validate(),
            Err(DomainError::InvalidSessionEndpoint)
        );

        let controller_role = RemoteCommand::AttachSession {
            role: SessionRole::Controller,
            controller: None,
            window: None,
        };
        assert!(controller_role.validate().is_ok());
    }

    #[test]
    fn test_attach_session_roundtrips_with_typed_endpoint() {
        let command: RemoteCommand = serde_json::from_str(
            r#"{"action":"attach_session","role":"remote","controller":{"address":"100.64.0.3","port":8765}}"#,
        )
        .expect("valid attach_session");

        assert!(command.validate().is_ok());
        assert_eq!(command.kind(), "attach_session");
        assert!(command.is_session_control());
    }

    #[test]
    fn test_switch_stream_display_validates_supported_monitor_names() {
        let command = RemoteCommand::SwitchStreamDisplay {
            display: DisplayId::from("HDMI-A-1"),
        };
        assert!(command.validate().is_ok());
        assert_eq!(command.kind(), "switch_stream_display");
        assert!(!command.is_session_control());
    }

    #[test]
    fn test_switch_stream_display_rejects_unsafe_monitor_identifiers() {
        let command = RemoteCommand::SwitchStreamDisplay {
            display: DisplayId::from("DP-1;rm -rf"),
        };
        assert_eq!(command.validate(), Err(DomainError::InvalidDisplayId));

        let empty = RemoteCommand::SwitchStreamDisplay {
            display: DisplayId::from(""),
        };
        assert_eq!(empty.validate(), Err(DomainError::InvalidDisplayId));
    }

    #[test]
    fn test_switch_stream_display_serializes_with_display_field() {
        let command: RemoteCommand =
            serde_json::from_str(r#"{"action":"switch_stream_display","display":"DP-2"}"#)
                .expect("valid switch_stream_display");

        assert!(command.validate().is_ok());
        match command {
            RemoteCommand::SwitchStreamDisplay { display } => {
                assert_eq!(display.as_str(), "DP-2");
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    fn topology(displays: &[&str], focused: Option<&str>) -> RemoteDesktopTopology {
        RemoteDesktopTopology::new(
            displays.iter().map(|id| DisplayId::from(*id)),
            focused.map(DisplayId::from),
        )
    }

    #[test]
    fn test_display_mode_serializes_as_stable_kebab_case() {
        assert_eq!(
            serde_json::to_string(&DisplayMode::FollowFocus).expect("serialize"),
            "\"follow-focus\""
        );
        assert_eq!(
            serde_json::from_str::<DisplayMode>("\"follow-focus\"").expect("deserialize"),
            DisplayMode::FollowFocus
        );
    }

    #[test]
    fn test_follow_focus_same_display_requests_no_switch() {
        let topology = topology(&["eDP-1", "DP-2"], Some("eDP-1"));

        assert_eq!(
            follow_focus_target(&topology, &DisplayId::from("eDP-1")),
            None
        );
    }

    #[test]
    fn test_follow_focus_switches_to_newly_focused_display() {
        let topology = topology(&["eDP-1", "DP-2"], Some("DP-2"));

        assert_eq!(
            follow_focus_target(&topology, &DisplayId::from("eDP-1")),
            Some(DisplayId::from("DP-2"))
        );
    }

    #[test]
    fn test_follow_focus_falls_back_when_streamed_display_disappears() {
        let topology = topology(&["eDP-1"], Some("eDP-1"));

        assert_eq!(
            follow_focus_target(&topology, &DisplayId::from("DP-2")),
            Some(DisplayId::from("eDP-1"))
        );
    }

    #[test]
    fn test_follow_focus_without_displays_requests_no_switch() {
        let topology = topology(&[], None);

        assert_eq!(
            follow_focus_target(&topology, &DisplayId::from("eDP-1")),
            None
        );
    }

    #[test]
    fn test_topology_from_displays_tracks_focused_monitor() {
        let displays = [
            Display {
                id: "eDP-1".to_owned(),
                name: "internal".to_owned(),
                width: 1920,
                height: 1080,
                refresh_hz: 60.0,
                focused: false,
            },
            Display {
                id: "DP-2".to_owned(),
                name: "external".to_owned(),
                width: 2560,
                height: 1440,
                refresh_hz: 144.0,
                focused: true,
            },
        ];

        let topology = RemoteDesktopTopology::from_displays(&displays);

        assert_eq!(topology.focused(), Some(&DisplayId::from("DP-2")));
        assert!(topology.contains(&DisplayId::from("eDP-1")));
    }
}
