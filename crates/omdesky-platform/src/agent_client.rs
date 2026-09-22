use async_trait::async_trait;
use omdesky_application::ports::{AgentClient, AgentEndpoint, PortError, PortResult};
use omdesky_core::{
    Display, Window, Workspace, WorkspaceTarget,
    text::{DISPLAY_LINE_LIMIT, DISPLAY_NAME_LIMIT, sanitize_for_display, sanitize_optional},
};
use omdesky_protocol::{
    ActiveWindowResponse, CommandRequest, CommandResponse, DisplaysResponse, ErrorEnvelope,
    FocusWorkspaceRequest, HealthResponse, NodeInfoResponse, SunshinePairChallengeResponse,
    SunshinePairRequest, SunshineStatusResponse, WindowsResponse, WorkspacesResponse,
};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const MAX_NODE_FIELD_BYTES: usize = 256;
const MAX_VERSION_BYTES: usize = 128;
const MAX_CAPABILITIES: usize = 128;
const MAX_CAPABILITY_BYTES: usize = 128;

#[derive(Clone, Default)]
pub struct HttpAgentClient {
    client: reqwest::Client,
}

impl HttpAgentClient {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }

    fn url(endpoint: &AgentEndpoint, path: &str) -> String {
        let address = if endpoint.address.is_ipv6() {
            format!("[{}]", endpoint.address)
        } else {
            endpoint.address.to_string()
        };
        format!("http://{address}:{}{path}", endpoint.port)
    }

    async fn parse<T: DeserializeOwned>(mut response: reqwest::Response) -> PortResult<T> {
        let status = response.status();
        let mut body = Vec::new();

        while let Some(chunk) = response.chunk().await.map_err(network_error)? {
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(PortError::new(
                    "AGENT_RESPONSE_LIMIT",
                    "the device response exceeded the maximum size",
                    false,
                ));
            }

            body.extend_from_slice(&chunk);
        }

        if !status.is_success() {
            let envelope = serde_json::from_slice::<ErrorEnvelope>(&body).ok();

            return Err(http_error(status, envelope));
        }

        serde_json::from_slice(&body).map_err(|error| {
            tracing::debug!(%status, detail = %error, "agent.response_invalid");
            PortError::new(
                "AGENT_PROTOCOL_INVALID",
                "the device returned an incompatible response",
                false,
            )
        })
    }

    async fn get<T: DeserializeOwned>(
        &self,
        endpoint: &AgentEndpoint,
        path: &str,
        timeout: Duration,
    ) -> PortResult<T> {
        Self::parse(
            self.client
                .get(Self::url(endpoint, path))
                .timeout(timeout)
                .send()
                .await
                .map_err(network_error)?,
        )
        .await
    }

    async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        endpoint: &AgentEndpoint,
        path: &str,
        body: &B,
    ) -> PortResult<T> {
        Self::parse(
            self.client
                .post(Self::url(endpoint, path))
                .timeout(REQUEST_TIMEOUT)
                .json(body)
                .send()
                .await
                .map_err(network_error)?,
        )
        .await
    }
}

#[async_trait]
impl AgentClient for HttpAgentClient {
    async fn health(&self, endpoint: &AgentEndpoint) -> PortResult<HealthResponse> {
        self.get(endpoint, "/v1/health", PROBE_TIMEOUT).await
    }

    async fn node_info(&self, endpoint: &AgentEndpoint) -> PortResult<NodeInfoResponse> {
        let response = self.get(endpoint, "/v1/node", REQUEST_TIMEOUT).await?;

        validate_node_info(response)
    }

    async fn displays(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Display>> {
        let response: DisplaysResponse =
            self.get(endpoint, "/v1/displays", REQUEST_TIMEOUT).await?;

        Ok(response
            .displays
            .into_iter()
            .map(sanitize_display)
            .collect())
    }

    async fn workspaces(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Workspace>> {
        let response: WorkspacesResponse = self
            .get(endpoint, "/v1/workspaces", REQUEST_TIMEOUT)
            .await?;

        Ok(response
            .workspaces
            .into_iter()
            .map(sanitize_workspace)
            .collect())
    }

    async fn windows(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Window>> {
        let response: WindowsResponse = self.get(endpoint, "/v1/windows", REQUEST_TIMEOUT).await?;

        Ok(response.windows.into_iter().map(sanitize_window).collect())
    }

    async fn active_window(&self, endpoint: &AgentEndpoint) -> PortResult<ActiveWindowResponse> {
        let response: ActiveWindowResponse = self
            .get(endpoint, "/v1/windows/active", REQUEST_TIMEOUT)
            .await?;

        Ok(ActiveWindowResponse {
            window: response.window.map(sanitize_window),
        })
    }

    async fn focus_workspace(
        &self,
        endpoint: &AgentEndpoint,
        target: WorkspaceTarget,
    ) -> PortResult<()> {
        let _: omdesky_protocol::FocusResponse = self
            .post_json(
                endpoint,
                "/v1/workspaces/focus",
                &FocusWorkspaceRequest { target },
            )
            .await?;
        Ok(())
    }

    async fn focus_window(&self, endpoint: &AgentEndpoint, window: &str) -> PortResult<()> {
        let _: omdesky_protocol::FocusResponse = self
            .post_json(
                endpoint,
                &format!("/v1/windows/{window}/focus"),
                &serde_json::json!({}),
            )
            .await?;
        Ok(())
    }

    async fn sunshine_status(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<SunshineStatusResponse> {
        self.get(endpoint, "/v1/sunshine", REQUEST_TIMEOUT).await
    }

    async fn sunshine_pair_challenge(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<SunshinePairChallengeResponse> {
        self.post_json(
            endpoint,
            "/v1/sunshine/pair/challenge",
            &serde_json::json!({}),
        )
        .await
    }

    async fn sunshine_pair(
        &self,
        endpoint: &AgentEndpoint,
        request: SunshinePairRequest,
    ) -> PortResult<()> {
        let _: omdesky_protocol::SunshinePairResponse = self
            .post_json(endpoint, "/v1/sunshine/pair", &request)
            .await?;
        Ok(())
    }

    async fn send_command(
        &self,
        endpoint: &AgentEndpoint,
        request: CommandRequest,
    ) -> PortResult<CommandResponse> {
        self.post_json(endpoint, "/v1/commands", &request).await
    }
}

fn sanitize_display(display: Display) -> Display {
    Display {
        id: sanitize_for_display(&display.id, DISPLAY_NAME_LIMIT),
        name: sanitize_for_display(&display.name, DISPLAY_NAME_LIMIT),
        ..display
    }
}

fn sanitize_workspace(workspace: Workspace) -> Workspace {
    Workspace {
        name: sanitize_optional(workspace.name.as_deref(), DISPLAY_NAME_LIMIT),
        monitor: sanitize_optional(workspace.monitor.as_deref(), DISPLAY_NAME_LIMIT),
        ..workspace
    }
}

fn sanitize_window(window: Window) -> Window {
    Window {
        app_id: sanitize_optional(window.app_id.as_deref(), DISPLAY_NAME_LIMIT),
        class: sanitize_optional(window.class.as_deref(), DISPLAY_NAME_LIMIT),
        title: sanitize_optional(window.title.as_deref(), DISPLAY_LINE_LIMIT),
        ..window
    }
}

fn sanitize_node_info(response: NodeInfoResponse) -> NodeInfoResponse {
    NodeInfoResponse {
        node_id: sanitize_for_display(&response.node_id, DISPLAY_NAME_LIMIT),
        hostname: sanitize_for_display(&response.hostname, DISPLAY_NAME_LIMIT),
        omarchy_version: sanitize_for_display(&response.omarchy_version, DISPLAY_NAME_LIMIT),
        agent_version: sanitize_for_display(&response.agent_version, DISPLAY_NAME_LIMIT),
        capabilities: response
            .capabilities
            .iter()
            .map(|capability| sanitize_for_display(capability, DISPLAY_NAME_LIMIT))
            .collect(),
        ..response
    }
}

fn validate_node_info(response: NodeInfoResponse) -> PortResult<NodeInfoResponse> {
    let fields_are_valid = response.node_id.len() <= MAX_NODE_FIELD_BYTES
        && response.hostname.len() <= MAX_NODE_FIELD_BYTES
        && response.omarchy_version.len() <= MAX_VERSION_BYTES
        && response.agent_version.len() <= MAX_VERSION_BYTES
        && response.protocol_versions.len() <= 32
        && response.capabilities.len() <= MAX_CAPABILITIES
        && response
            .capabilities
            .iter()
            .all(|capability| capability.len() <= MAX_CAPABILITY_BYTES);

    if !fields_are_valid {
        return Err(PortError::new(
            "AGENT_FIELD_LIMIT",
            "the device response contained an oversized field",
            false,
        ));
    }

    Ok(sanitize_node_info(response))
}

fn network_error(error: reqwest::Error) -> PortError {
    tracing::debug!(
        detail = %error,
        timeout = error.is_timeout(),
        "agent.request_failed"
    );

    PortError::new("AGENT_UNREACHABLE", "the device could not be reached", true)
}

fn http_error(status: StatusCode, envelope: Option<ErrorEnvelope>) -> PortError {
    tracing::debug!(
        %status,
        remote_code = ?envelope.as_ref().map(|value| value.error.code.as_str()),
        "agent.http_error"
    );

    envelope.map_or_else(
        || {
            PortError::new(
                "AGENT_REQUEST_FAILED",
                format!("agent returned HTTP {status}"),
                status.is_server_error(),
            )
        },
        |envelope| {
            let code = match envelope.error.code.as_str() {
                "UNAUTHORIZED" => "UNAUTHORIZED",
                "OMARCHY_UNSUPPORTED" => "OMARCHY_UNSUPPORTED",
                "HYPRLAND_UNAVAILABLE" => "HYPRLAND_UNAVAILABLE",
                "DISPLAY_NOT_FOUND" => "DISPLAY_NOT_FOUND",
                "WORKSPACE_NOT_FOUND" => "WORKSPACE_NOT_FOUND",
                "WINDOW_NOT_FOUND" => "WINDOW_NOT_FOUND",
                "SUNSHINE_NOT_INSTALLED" => "SUNSHINE_NOT_INSTALLED",
                "SUNSHINE_NOT_RUNNING" => "SUNSHINE_NOT_RUNNING",
                "SUNSHINE_API_UNAVAILABLE" => "SUNSHINE_API_UNAVAILABLE",
                "SUNSHINE_PAIRING_FAILED" => "SUNSHINE_PAIRING_FAILED",
                "INVALID_COMMAND" => "INVALID_COMMAND",
                _ => "AGENT_REQUEST_FAILED",
            };

            PortError::new(code, envelope.error.message, envelope.error.retryable)
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_protocol::ProtocolError;
    use serde_json::Map;

    #[tokio::test]
    async fn test_parse_rejects_oversized_response_body() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let address = listener.local_addr().expect("listener address");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("connection accepted");
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request).await;
            let body = vec![b'x'; MAX_RESPONSE_BYTES + 1];
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("headers written");
            stream.write_all(&body).await.expect("body written");
        });
        let endpoint = AgentEndpoint {
            address: address.ip(),
            port: address.port(),
        };

        let error = HttpAgentClient::new()
            .health(&endpoint)
            .await
            .expect_err("oversized body fails");
        server.await.expect("server exits");

        assert_eq!(error.code, "AGENT_RESPONSE_LIMIT");
    }

    #[test]
    fn test_validate_node_info_rejects_oversized_fields() {
        let response = NodeInfoResponse {
            node_id: "node".to_owned(),
            hostname: "x".repeat(MAX_NODE_FIELD_BYTES + 1),
            omarchy_version: "4.0.0".to_owned(),
            agent_version: "0.1.0".to_owned(),
            protocol_versions: vec![1],
            capabilities: Vec::new(),
        };

        let error = validate_node_info(response).expect_err("oversized field fails");

        assert_eq!(error.code, "AGENT_FIELD_LIMIT");
    }

    #[test]
    fn test_validate_node_info_rejects_too_many_capabilities() {
        let response = NodeInfoResponse {
            node_id: "node".to_owned(),
            hostname: "host".to_owned(),
            omarchy_version: "4.0.0".to_owned(),
            agent_version: "0.1.0".to_owned(),
            protocol_versions: vec![1],
            capabilities: vec!["capability".to_owned(); MAX_CAPABILITIES + 1],
        };

        let error = validate_node_info(response).expect_err("too many capabilities fail");

        assert_eq!(error.code, "AGENT_FIELD_LIMIT");
    }

    #[test]
    fn test_remote_metadata_cannot_carry_terminal_control_sequences() {
        let response = sanitize_node_info(NodeInfoResponse {
            node_id: "node".to_owned(),
            hostname: "desk\u{1b}]8;;https://evil.example\u{7}top".to_owned(),
            omarchy_version: "4.0.0\u{202e}".to_owned(),
            agent_version: "0.1.1".to_owned(),
            protocol_versions: vec![1],
            capabilities: vec!["desktop.input\u{1b}[2J".to_owned()],
        });

        assert!(!response.hostname.contains('\u{1b}'));
        assert!(!response.hostname.contains('\u{7}'));
        assert!(!response.omarchy_version.contains('\u{202e}'));
        assert!(!response.capabilities[0].contains('\u{1b}'));
    }

    #[test]
    fn test_remote_window_titles_are_sanitized_but_handles_are_preserved() {
        let window = sanitize_window(Window {
            id: omdesky_core::WindowId("0x55aa".to_owned()),
            app_id: Some("code\u{1b}[31m".to_owned()),
            class: None,
            title: Some("main\u{202e}txt.exe".to_owned()),
            workspace: omdesky_core::WorkspaceId(1),
            focused: false,
        });

        assert_eq!(window.id, omdesky_core::WindowId("0x55aa".to_owned()));
        assert!(!window.app_id.expect("app id").contains('\u{1b}'));
        assert!(!window.title.expect("title").contains('\u{202e}'));
    }

    #[test]
    fn test_remote_display_names_are_sanitized() {
        let display = sanitize_display(Display {
            id: "DP-2".to_owned(),
            name: "external\u{1b}[2J".to_owned(),
            width: 2560,
            height: 1440,
            refresh_hz: 144.0,
            focused: true,
        });

        assert_eq!(display.id, "DP-2");
        assert!(!display.name.contains('\u{1b}'));
    }

    #[test]
    fn test_http_error_keeps_public_message_user_friendly() {
        let error = http_error(
            StatusCode::SERVICE_UNAVAILABLE,
            Some(ErrorEnvelope {
                error: ProtocolError {
                    code: "HYPRLAND_UNAVAILABLE".to_owned(),
                    message: "socket /run/user/1000/hypr/private is missing".to_owned(),
                    retryable: true,
                    details: Map::new(),
                },
            }),
        );

        assert_eq!(
            error.to_string(),
            "The desktop could not be controlled. Make sure Hyprland is running."
        );
        assert!(!error.to_string().contains("/run/user"));
    }
}
