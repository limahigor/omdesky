#![forbid(unsafe_code)]

pub mod release;

pub use release::{RELEASE, RELEASE_HEADER, ReleaseLine, is_compatible_release};

use omdesky_core::{
    ControlCapability, Display, RemoteCommand, SessionClaim, SessionGrant, Window, Workspace,
    WorkspaceTarget,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use time::OffsetDateTime;
use uuid::Uuid;

pub const PROTOCOL_V1: u16 = 1;
pub const SUPPORTED_PROTOCOLS: &[u16] = &[PROTOCOL_V1];

pub const MAX_PAIRING_ID_BYTES: usize = 64;
pub const MAX_PIN_BYTES: usize = 16;
pub const MAX_CLIENT_NAME_BYTES: usize = 64;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    pub protocol: u16,
    pub agent_version: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NodeInfoResponse {
    pub node_id: String,
    pub hostname: String,
    pub omarchy_version: String,
    pub agent_version: String,
    pub protocol_versions: Vec<u16>,
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct DisplaysResponse {
    pub displays: Vec<Display>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WorkspacesResponse {
    pub workspaces: Vec<Workspace>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct WindowsResponse {
    pub windows: Vec<Window>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ActiveWindowResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<Window>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FocusWorkspaceRequest {
    pub target: WorkspaceTarget,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FocusResponse {
    pub focused: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandRequest {
    pub command: RemoteCommand,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionClaim>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<Uuid>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[serde(with = "time::serde::rfc3339::option")]
    pub issued_at: Option<OffsetDateTime>,
}

impl CommandRequest {
    pub fn new(command: RemoteCommand) -> Self {
        Self {
            command,
            session: None,
            request_id: Some(Uuid::new_v4()),
            issued_at: Some(OffsetDateTime::now_utc()),
        }
    }

    pub fn for_session(command: RemoteCommand, session: SessionClaim) -> Self {
        Self {
            session: Some(session),
            ..Self::new(command)
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub executed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionGrant>,
}

impl CommandResponse {
    pub fn executed() -> Self {
        Self {
            executed: true,
            session: None,
        }
    }

    pub fn ignored() -> Self {
        Self {
            executed: false,
            session: None,
        }
    }

    pub fn granted(session: SessionGrant) -> Self {
        Self {
            executed: true,
            session: Some(session),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SunshineStatusResponse {
    pub installed: bool,
    pub running: bool,
    pub ready: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_app: Option<String>,
    pub pairing_api_available: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SunshinePairChallengeResponse {
    pub pairing_id: String,
    pub expires_in_seconds: u64,
}

#[derive(Clone, Eq, PartialEq, Serialize, Deserialize)]
pub struct SunshinePairRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_id: Option<String>,
    pub pin: String,
    pub client_name: String,
}

impl SunshinePairRequest {
    pub fn validate(&self) -> Result<(), PairingFieldError> {
        if !(self.pin.len() == 4 && self.pin.chars().all(|digit| digit.is_ascii_digit())) {
            return Err(PairingFieldError::Pin);
        }

        let client_name_valid = !self.client_name.is_empty()
            && self.client_name.len() <= MAX_CLIENT_NAME_BYTES
            && self
                .client_name
                .chars()
                .all(|character| character.is_ascii_graphic() || character == ' ');

        if !client_name_valid {
            return Err(PairingFieldError::ClientName);
        }

        let pairing_id_valid = self.pairing_id.as_ref().is_none_or(|id| {
            !id.is_empty()
                && id.len() <= MAX_PAIRING_ID_BYTES
                && id
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric() || character == '-')
        });

        if !pairing_id_valid {
            return Err(PairingFieldError::PairingId);
        }

        Ok(())
    }
}

impl std::fmt::Debug for SunshinePairRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SunshinePairRequest")
            .field("pairing_id", &self.pairing_id)
            .field("pin", &"<redacted>")
            .field("client_name", &self.client_name)
            .finish()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingFieldError {
    Pin,
    ClientName,
    PairingId,
}

impl PairingFieldError {
    pub fn code(self) -> &'static str {
        match self {
            PairingFieldError::Pin => "INVALID_PAIRING_PIN",
            PairingFieldError::ClientName => "INVALID_PAIRING_CLIENT",
            PairingFieldError::PairingId => "INVALID_PAIRING_CHALLENGE",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SunshinePairResponse {
    pub paired: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CapabilitiesResponse {
    pub capabilities: Vec<ControlCapability>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ErrorEnvelope {
    pub error: ProtocolError,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProtocolError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
    #[serde(default)]
    pub details: Map<String, Value>,
}

pub fn negotiate_protocol(client: &[u16], agent: &[u16]) -> Option<u16> {
    client
        .iter()
        .filter(|version| agent.contains(version))
        .max()
        .copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_core::{WindowId, WorkspaceId};

    #[test]
    fn test_negotiate_protocol_selects_highest_common_version() {
        assert_eq!(negotiate_protocol(&[1, 2, 3], &[1, 2]), Some(2));
    }

    #[test]
    fn test_negotiate_protocol_rejects_incompatible_versions() {
        assert_eq!(negotiate_protocol(&[2], &[1]), None);
    }

    #[test]
    fn test_health_ignores_unknown_fields() {
        let response: HealthResponse = serde_json::from_str(
            r#"{"status":"ok","protocol":1,"agent_version":"0.1.0","future":true}"#,
        )
        .expect("valid response");

        assert_eq!(response.protocol, 1);
    }

    #[test]
    fn test_unknown_capabilities_survive_roundtrip() {
        let response: NodeInfoResponse = serde_json::from_str(
            r#"{"node_id":"node","hostname":"host","omarchy_version":"4.0.1","agent_version":"0.1.0","protocol_versions":[1],"capabilities":["desktop.stream-host","future.value"]}"#,
        )
        .expect("valid node info");
        let encoded = serde_json::to_string(&response).expect("serializable node info");

        assert!(encoded.contains("future.value"));
    }

    #[test]
    fn test_error_envelope_has_stable_fields() {
        let error = ErrorEnvelope {
            error: ProtocolError {
                code: "SUNSHINE_NOT_READY".to_owned(),
                message: "Sunshine is not ready".to_owned(),
                retryable: false,
                details: Map::new(),
            },
        };
        let value = serde_json::to_value(error).expect("serializable error");

        assert_eq!(value["error"]["code"], "SUNSHINE_NOT_READY");
    }

    #[test]
    fn test_active_window_response_roundtrips_empty() {
        let response: ActiveWindowResponse =
            serde_json::from_str("{}").expect("valid empty active window");

        assert!(response.window.is_none());
    }

    #[test]
    fn test_focus_workspace_request_encodes_typed_selector() {
        let request = FocusWorkspaceRequest {
            target: WorkspaceTarget::Id(WorkspaceId(2)),
        };
        let value = serde_json::to_value(&request).expect("serializable request");

        assert_eq!(value["target"]["id"], 2);
    }

    #[test]
    fn test_windows_response_uses_stable_handles() {
        let response = WindowsResponse {
            windows: vec![Window {
                id: WindowId("0x55".to_owned()),
                app_id: Some("code".to_owned()),
                class: None,
                title: Some("main".to_owned()),
                workspace: WorkspaceId(2),
                focused: true,
            }],
        };
        let value = serde_json::to_value(&response).expect("serializable windows");

        assert_eq!(value["windows"][0]["id"], "0x55");
    }

    #[test]
    fn test_command_request_roundtrips_typed_shortcut() {
        let request: CommandRequest = serde_json::from_str(
            r#"{"command":{"action":"send_shortcut","chord":{"mods":["ctrl","alt","shift"],"key":"Z"},"window":{"class":"com.moonlight_stream.Moonlight"}}}"#,
        )
        .expect("valid command request");
        let value = serde_json::to_value(&request).expect("serializable command request");

        assert_eq!(value["command"]["action"], "send_shortcut");
        assert_eq!(value["command"]["chord"]["key"], "Z");
    }

    #[test]
    fn test_command_request_carries_freshness_and_session_claim() {
        let claim = omdesky_core::SessionClaim {
            id: omdesky_core::SessionId::new(),
            generation: 3,
        };
        let request = CommandRequest::for_session(RemoteCommand::DetachSession, claim);
        let value = serde_json::to_value(&request).expect("serializable request");

        assert_eq!(value["session"]["generation"], 3);
        assert!(value["request_id"].is_string());
        assert!(value["issued_at"].is_string());

        let decoded: CommandRequest =
            serde_json::from_value(value).expect("round trips through the wire format");
        assert_eq!(decoded.session, Some(claim));
    }

    #[test]
    fn test_command_request_tolerates_a_bare_command() {
        let request: CommandRequest =
            serde_json::from_str(r#"{"command":{"action":"detach_session"}}"#)
                .expect("bare command still parses");

        assert_eq!(request.session, None);
        assert_eq!(request.request_id, None);
        assert_eq!(request.issued_at, None);
    }

    #[test]
    fn test_command_response_reports_a_session_grant() {
        let grant = omdesky_core::SessionGrant {
            id: omdesky_core::SessionId::new(),
            generation: 1,
            lease_seconds: 30,
        };
        let value = serde_json::to_value(CommandResponse::granted(grant)).expect("serializable");

        assert_eq!(value["executed"], true);
        assert_eq!(value["session"]["lease_seconds"], 30);
        assert!(
            serde_json::to_value(CommandResponse::ignored())
                .expect("serializable")
                .get("session")
                .is_none()
        );
    }

    #[test]
    fn test_pairing_request_rejects_malformed_fields() {
        let valid = SunshinePairRequest {
            pairing_id: Some("ab-12".to_owned()),
            pin: "1234".to_owned(),
            client_name: "desktop-a".to_owned(),
        };
        assert_eq!(valid.validate(), Ok(()));

        let long_pin = SunshinePairRequest {
            pin: "12345".to_owned(),
            ..valid.clone()
        };
        assert_eq!(long_pin.validate(), Err(PairingFieldError::Pin));

        let hostile_name = SunshinePairRequest {
            client_name: "desk\u{1b}[2Jtop".to_owned(),
            ..valid.clone()
        };
        assert_eq!(hostile_name.validate(), Err(PairingFieldError::ClientName));

        let hostile_challenge = SunshinePairRequest {
            pairing_id: Some("../../etc".to_owned()),
            ..valid
        };
        assert_eq!(
            hostile_challenge.validate(),
            Err(PairingFieldError::PairingId)
        );
    }

    #[test]
    fn test_pairing_request_debug_never_reveals_the_pin() {
        let request = SunshinePairRequest {
            pairing_id: None,
            pin: "4821".to_owned(),
            client_name: "desktop-a".to_owned(),
        };

        let rendered = format!("{request:?}");

        assert!(!rendered.contains("4821"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn test_command_request_rejects_unknown_action() {
        let result = serde_json::from_str::<CommandRequest>(
            r#"{"command":{"action":"exec","cmd":"rm -rf /"}}"#,
        );

        assert!(result.is_err());
    }
}
