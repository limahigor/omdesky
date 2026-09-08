use omdesk_core::{Display, RemoteCommand, Window, Workspace, WorkspaceTarget};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const PROTOCOL_V1: u16 = 1;
pub const SUPPORTED_PROTOCOLS: &[u16] = &[PROTOCOL_V1];

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
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CommandResponse {
    pub executed: bool,
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
pub struct SunshinePairRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pairing_id: Option<String>,
    pub pin: String,
    pub client_name: String,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SunshinePairResponse {
    pub paired: bool,
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
    use omdesk_core::{WindowId, WorkspaceId};

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
    fn test_command_request_rejects_unknown_action() {
        let result = serde_json::from_str::<CommandRequest>(
            r#"{"command":{"action":"exec","cmd":"rm -rf /"}}"#,
        );

        assert!(result.is_err());
    }
}
