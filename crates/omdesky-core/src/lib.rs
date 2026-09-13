use serde::{Deserialize, Serialize};
use std::{fmt, net::IpAddr, str::FromStr};
use thiserror::Error;
use time::OffsetDateTime;
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeAlias(String);

impl NodeAlias {
    pub fn new(value: impl Into<String>) -> Result<Self, DomainError> {
        let value = value.into();
        let trimmed = value.trim();
        if trimmed.is_empty() || trimmed.len() > 64 {
            return Err(DomainError::InvalidNodeAlias);
        }

        Ok(Self(trimmed.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for NodeAlias {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TailnetIdentity {
    pub node_id: String,
    pub user: Option<String>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    Ready,
    Offline,
    AgentUnknown,
    Incompatible,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Node {
    pub id: NodeId,
    pub hostname: String,
    pub alias: Option<NodeAlias>,
    pub tailnet: TailnetIdentity,
    pub status: NodeStatus,
    pub capabilities: NodeCapabilities,
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WindowTarget {
    pub id: WindowId,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DisplaySelection {
    Automatic,
    Id(String),
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
        }
    }

    pub fn as_hypr(&self) -> String {
        match self {
            WindowSelector::ActiveWindow => "activewindow".to_owned(),
            WindowSelector::Class(class) => format!("class:{class}"),
        }
    }
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
    },
    DetachSession,
}

impl RemoteCommand {
    pub fn kind(&self) -> &'static str {
        match self {
            RemoteCommand::SendShortcut { .. } => "send_shortcut",
            RemoteCommand::CloseWindow { .. } => "close_window",
            RemoteCommand::SwitchStreamDisplay { .. } => "switch_stream_display",
            RemoteCommand::AttachSession { .. } => "attach_session",
            RemoteCommand::DetachSession => "detach_session",
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
            RemoteCommand::AttachSession { role, controller } => match role {
                SessionRole::Remote => controller
                    .as_ref()
                    .ok_or(DomainError::InvalidSessionEndpoint)
                    .and_then(SessionEndpoint::validate),
                SessionRole::Controller => Ok(()),
            },
            RemoteCommand::DetachSession => Ok(()),
        }
    }

    pub fn is_session_control(&self) -> bool {
        matches!(
            self,
            RemoteCommand::AttachSession { .. } | RemoteCommand::DetachSession
        )
    }

    pub fn controller_exclusive(&self) -> bool {
        match self {
            RemoteCommand::SendShortcut { .. }
            | RemoteCommand::CloseWindow { .. }
            | RemoteCommand::SwitchStreamDisplay { .. } => false,
            RemoteCommand::AttachSession { .. } | RemoteCommand::DetachSession => false,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionState {
    Idle,
    ResolvingNode,
    CheckingRemote,
    CheckingStreamPairing,
    Pairing,
    PreparingRemote,
    LaunchingMoonlight,
    Connected,
    Stopping,
    Cleanup,
    Failed,
}

impl SessionState {
    pub fn transition(self, next: Self) -> Result<Self, DomainError> {
        use SessionState::*;
        let valid = matches!(
            (self, next),
            (Idle, ResolvingNode)
                | (ResolvingNode, CheckingRemote)
                | (CheckingRemote, CheckingStreamPairing)
                | (CheckingStreamPairing, Pairing | PreparingRemote)
                | (Pairing, PreparingRemote)
                | (PreparingRemote, LaunchingMoonlight)
                | (LaunchingMoonlight, Connected)
                | (Connected, Stopping)
                | (Stopping, Cleanup)
                | (Failed, Cleanup)
                | (Cleanup, Idle)
        ) || matches!(
            self,
            ResolvingNode
                | CheckingRemote
                | CheckingStreamPairing
                | Pairing
                | PreparingRemote
                | LaunchingMoonlight
                | Connected
        ) && next == Failed;

        valid
            .then_some(next)
            .ok_or(DomainError::InvalidSessionTransition {
                from: self,
                to: next,
            })
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DesktopSession {
    pub id: SessionId,
    pub remote_node: String,
    pub moonlight_pid: Option<u32>,
    pub state: SessionState,
    pub input_mode: InputMode,
    pub target_display: Option<String>,
    pub target_workspace: Option<WorkspaceId>,
    pub started_at: OffsetDateTime,
}

impl DesktopSession {
    pub fn transition(&mut self, next: SessionState) -> Result<(), DomainError> {
        self.state = self.state.transition(next)?;
        Ok(())
    }
}

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum DomainError {
    #[error("node alias must contain 1 to 64 non-whitespace characters")]
    InvalidNodeAlias,
    #[error("invalid session transition from {from:?} to {to:?}")]
    InvalidSessionTransition {
        from: SessionState,
        to: SessionState,
    },
    #[error("key chord key must be a short ASCII-alphanumeric token")]
    InvalidKeyChord,
    #[error("window selector is not a safe class token")]
    InvalidWindowSelector,
    #[error("session endpoint requires a controller address and non-zero port")]
    InvalidSessionEndpoint,
    #[error("display identifier is not a safe monitor token")]
    InvalidDisplayId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_alias_rejects_empty_input() {
        assert_eq!(NodeAlias::new("  "), Err(DomainError::InvalidNodeAlias));
    }

    #[test]
    fn test_capabilities_preserve_unknown_values() {
        let capabilities = NodeCapabilities::new([
            "desktop.stream-host".to_owned(),
            "future.capability".to_owned(),
        ]);

        assert!(capabilities.contains("future.capability"));
    }

    #[test]
    fn test_session_state_accepts_full_connect_lifecycle() {
        let states = [
            SessionState::ResolvingNode,
            SessionState::CheckingRemote,
            SessionState::CheckingStreamPairing,
            SessionState::PreparingRemote,
            SessionState::LaunchingMoonlight,
            SessionState::Connected,
            SessionState::Stopping,
            SessionState::Cleanup,
            SessionState::Idle,
        ];
        let final_state = states
            .into_iter()
            .try_fold(SessionState::Idle, SessionState::transition)
            .expect("valid lifecycle");

        assert_eq!(final_state, SessionState::Idle);
    }

    #[test]
    fn test_session_state_allows_optional_pairing_step() {
        assert_eq!(
            SessionState::CheckingStreamPairing.transition(SessionState::Pairing),
            Ok(SessionState::Pairing)
        );
    }

    #[test]
    fn test_session_state_rejects_invalid_transition() {
        assert_eq!(
            SessionState::Idle.transition(SessionState::Connected),
            Err(DomainError::InvalidSessionTransition {
                from: SessionState::Idle,
                to: SessionState::Connected,
            })
        );
    }

    #[test]
    fn test_failed_session_requires_cleanup() {
        assert!(SessionState::Failed.transition(SessionState::Idle).is_err());
        assert_eq!(
            SessionState::Failed.transition(SessionState::Cleanup),
            Ok(SessionState::Cleanup)
        );
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
        };
        assert_eq!(
            remote_without_controller.validate(),
            Err(DomainError::InvalidSessionEndpoint)
        );

        let controller_role = RemoteCommand::AttachSession {
            role: SessionRole::Controller,
            controller: None,
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
        assert!(!command.controller_exclusive());
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
