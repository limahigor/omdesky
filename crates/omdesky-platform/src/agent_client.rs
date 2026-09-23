use async_trait::async_trait;
use omdesky_application::ports::{AgentClient, AgentEndpoint, PortError, PortResult};
use omdesky_core::{
    ControlCapability, Display, Window, Workspace, WorkspaceTarget,
    text::{DISPLAY_LINE_LIMIT, DISPLAY_NAME_LIMIT, sanitize_for_display, sanitize_optional},
};
use omdesky_protocol::{
    ActiveWindowResponse, CapabilitiesResponse, CommandRequest, CommandResponse, DisplaysResponse,
    ErrorEnvelope, FocusWorkspaceRequest, HealthResponse, NodeInfoResponse, RELEASE,
    RELEASE_HEADER, SunshinePairChallengeResponse, SunshinePairRequest, SunshineStatusResponse,
    WindowsResponse, WorkspacesResponse, is_compatible_release,
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
        let response = self
            .client
            .get(Self::url(endpoint, path))
            .header(RELEASE_HEADER, RELEASE)
            .timeout(timeout)
            .send()
            .await
            .map_err(network_error)?;

        verify_release(&response)?;

        Self::parse(response).await
    }

    async fn post_json<B: serde::Serialize, T: DeserializeOwned>(
        &self,
        endpoint: &AgentEndpoint,
        path: &str,
        body: &B,
    ) -> PortResult<T> {
        let response = self
            .client
            .post(Self::url(endpoint, path))
            .header(RELEASE_HEADER, RELEASE)
            .timeout(REQUEST_TIMEOUT)
            .json(body)
            .send()
            .await
            .map_err(network_error)?;

        verify_release(&response)?;

        Self::parse(response).await
    }
}

#[async_trait]
impl AgentClient for HttpAgentClient {
    async fn health(&self, endpoint: &AgentEndpoint) -> PortResult<HealthResponse> {
        let response = self
            .client
            .get(Self::url(endpoint, "/v1/health"))
            .header(RELEASE_HEADER, RELEASE)
            .timeout(PROBE_TIMEOUT)
            .send()
            .await
            .map_err(network_error)?;

        let health: HealthResponse = Self::parse(response).await?;

        if health.agent_version.len() > MAX_VERSION_BYTES {
            return Err(PortError::new(
                "AGENT_FIELD_LIMIT",
                "the device response contained an oversized field",
                false,
            ));
        }

        Ok(HealthResponse {
            agent_version: sanitize_for_display(&health.agent_version, DISPLAY_NAME_LIMIT),
            ..health
        })
    }

    async fn node_info(&self, endpoint: &AgentEndpoint) -> PortResult<NodeInfoResponse> {
        let response = self.get(endpoint, "/v1/node", REQUEST_TIMEOUT).await?;

        validate_node_info(response)
    }

    async fn granted_capabilities(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<Vec<ControlCapability>> {
        let response: CapabilitiesResponse = self
            .get(endpoint, "/v1/capabilities", REQUEST_TIMEOUT)
            .await?;

        Ok(response.capabilities)
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
    }
}

fn validate_node_info(response: NodeInfoResponse) -> PortResult<NodeInfoResponse> {
    let fields_are_valid = response.node_id.len() <= MAX_NODE_FIELD_BYTES
        && response.hostname.len() <= MAX_NODE_FIELD_BYTES
        && response.omarchy_version.len() <= MAX_VERSION_BYTES
        && response.agent_version.len() <= MAX_VERSION_BYTES
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

fn verify_release(response: &reqwest::Response) -> PortResult<()> {
    let release = response
        .headers()
        .get(RELEASE_HEADER)
        .and_then(|value| value.to_str().ok());

    match release {
        Some(release) if is_compatible_release(release) => Ok(()),
        release => Err(release_mismatch(release)),
    }
}

fn release_mismatch(remote: Option<&str>) -> PortError {
    let remote = remote.map_or_else(
        || "an earlier release".to_owned(),
        |release| sanitize_for_display(release, DISPLAY_NAME_LIMIT),
    );

    PortError::new(
        "VERSION_INCOMPATIBLE",
        format!("the device runs Omdesky {remote} and this computer runs {RELEASE}"),
        false,
    )
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
    let known = envelope
        .as_ref()
        .and_then(|envelope| envelope.error.known_code());

    tracing::debug!(
        %status,
        remote_code = ?envelope.as_ref().map(|value| value.error.code.as_str()),
        "agent.http_error"
    );

    match (known, envelope) {
        (Some(code), Some(envelope)) => PortError::new(
            code.as_str(),
            envelope.error.message,
            envelope.error.retryable,
        ),
        (None, Some(envelope)) => PortError::new(
            "AGENT_REQUEST_FAILED",
            envelope.error.message,
            envelope.error.retryable,
        ),
        (_, None) => PortError::new(
            "AGENT_REQUEST_FAILED",
            format!("agent returned HTTP {status}"),
            status.is_server_error(),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_protocol::ProtocolError;

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

    async fn serve_once(
        release: Option<String>,
        body: String,
    ) -> (AgentEndpoint, tokio::task::JoinHandle<String>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let address = listener.local_addr().expect("listener address");

        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("connection accepted");

            let mut request = vec![0_u8; 4096];
            let read = stream.read(&mut request).await.expect("request read");

            let release_line = release
                .map(|release| format!("{RELEASE_HEADER}: {release}\r\n"))
                .unwrap_or_default();
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n{release_line}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );

            stream
                .write_all(response.as_bytes())
                .await
                .expect("response written");

            String::from_utf8_lossy(&request[..read]).into_owned()
        });

        let endpoint = AgentEndpoint {
            address: address.ip(),
            port: address.port(),
        };

        (endpoint, server)
    }

    fn node_body() -> String {
        serde_json::json!({
            "node_id": "node",
            "hostname": "host",
            "omarchy_version": "4.0.0",
            "agent_version": RELEASE,
            "capabilities": [],
        })
        .to_string()
    }

    fn health_body(agent_version: &str) -> String {
        serde_json::json!({
            "status": "ok",
            "protocol": 1,
            "agent_version": agent_version,
        })
        .to_string()
    }

    fn other_minor_release() -> String {
        let current = omdesky_protocol::ReleaseLine::current().expect("current release parses");

        format!("{}.{}.0", current.major, current.minor + 1)
    }

    #[tokio::test]
    async fn test_requests_announce_the_controller_release() {
        let (endpoint, server) = serve_once(Some(RELEASE.to_owned()), node_body()).await;

        HttpAgentClient::new()
            .node_info(&endpoint)
            .await
            .expect("a matching release is accepted");

        let request = server.await.expect("server exits").to_ascii_lowercase();

        assert!(request.contains(&format!("{RELEASE_HEADER}: {RELEASE}")));
    }

    #[tokio::test]
    async fn test_an_agent_without_a_release_header_is_incompatible() {
        let (endpoint, server) = serve_once(None, node_body()).await;

        let error = HttpAgentClient::new()
            .node_info(&endpoint)
            .await
            .expect_err("an earlier agent is refused");
        server.await.expect("server exits");

        assert_eq!(error.code, "VERSION_INCOMPATIBLE");
    }

    #[tokio::test]
    async fn test_an_agent_from_another_release_line_is_incompatible() {
        let (endpoint, server) = serve_once(Some(other_minor_release()), node_body()).await;

        let error = HttpAgentClient::new()
            .node_info(&endpoint)
            .await
            .expect_err("another release line is refused");
        server.await.expect("server exits");

        assert_eq!(error.code, "VERSION_INCOMPATIBLE");
    }

    #[tokio::test]
    async fn test_granted_capabilities_ignore_unknown_values() {
        let body = serde_json::json!({
            "capabilities": ["send_shortcut", "future_capability", "close_stream"],
        })
        .to_string();
        let (endpoint, server) = serve_once(Some(RELEASE.to_owned()), body).await;

        let granted = HttpAgentClient::new()
            .granted_capabilities(&endpoint)
            .await
            .expect("grants are readable");
        let request = server.await.expect("server exits");

        assert!(request.starts_with("GET /v1/capabilities "));
        assert_eq!(granted, ControlCapability::CALLBACK.to_vec());
    }

    #[tokio::test]
    async fn test_health_reports_an_agent_from_another_release_line() {
        let other = other_minor_release();
        let (endpoint, server) = serve_once(None, health_body(&other)).await;

        let health = HttpAgentClient::new()
            .health(&endpoint)
            .await
            .expect("health answers every release");
        server.await.expect("server exits");

        assert_eq!(health.agent_version, other);
    }

    #[tokio::test]
    async fn test_health_accepts_an_agent_from_the_same_release_line() {
        let (endpoint, server) = serve_once(None, health_body(RELEASE)).await;

        let health = HttpAgentClient::new()
            .health(&endpoint)
            .await
            .expect("the same release line is accepted");
        server.await.expect("server exits");

        assert_eq!(health.agent_version, RELEASE);
    }

    #[test]
    fn test_validate_node_info_rejects_oversized_fields() {
        let response = NodeInfoResponse {
            node_id: "node".to_owned(),
            hostname: "x".repeat(MAX_NODE_FIELD_BYTES + 1),
            omarchy_version: "4.0.0".to_owned(),
            agent_version: "0.1.0".to_owned(),
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
    fn test_http_error_preserves_every_known_wire_code() {
        for code in omdesky_protocol::ErrorCode::ALL {
            let error = http_error(
                StatusCode::CONFLICT,
                Some(ErrorEnvelope::new(*code, "detail")),
            );

            assert_eq!(error.code, code.as_str());
            assert_eq!(error.retryable, code.retryable());
        }
    }

    #[test]
    fn test_http_error_reports_an_unknown_wire_code_as_a_failed_request() {
        let mut envelope = ErrorEnvelope::new(omdesky_protocol::ErrorCode::Internal, "detail");
        envelope.error.code = "FUTURE_ERROR".to_owned();

        let error = http_error(StatusCode::CONFLICT, Some(envelope));

        assert_eq!(error.code, "AGENT_REQUEST_FAILED");
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
