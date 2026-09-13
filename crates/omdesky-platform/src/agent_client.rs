use async_trait::async_trait;
use omdesky_application::ports::{AgentClient, AgentEndpoint, PortError, PortResult};
use omdesky_core::{Display, RemoteCommand, Window, Workspace, WorkspaceTarget};
use omdesky_protocol::{
    ActiveWindowResponse, CommandRequest, DisplaysResponse, ErrorEnvelope, FocusWorkspaceRequest,
    HealthResponse, NodeInfoResponse, SunshinePairRequest, SunshineStatusResponse, WindowsResponse,
    WorkspacesResponse,
};
use reqwest::StatusCode;
use serde::de::DeserializeOwned;
use std::time::Duration;

const PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

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

    async fn parse<T: DeserializeOwned>(response: reqwest::Response) -> PortResult<T> {
        let status = response.status();

        if !status.is_success() {
            let envelope = response.json::<ErrorEnvelope>().await.ok();
            return Err(http_error(status, envelope));
        }

        response.json().await.map_err(|error| {
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
        self.get(endpoint, "/v1/node", REQUEST_TIMEOUT).await
    }

    async fn displays(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Display>> {
        let response: DisplaysResponse =
            self.get(endpoint, "/v1/displays", REQUEST_TIMEOUT).await?;
        Ok(response.displays)
    }

    async fn workspaces(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Workspace>> {
        let response: WorkspacesResponse = self
            .get(endpoint, "/v1/workspaces", REQUEST_TIMEOUT)
            .await?;
        Ok(response.workspaces)
    }

    async fn windows(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Window>> {
        let response: WindowsResponse = self.get(endpoint, "/v1/windows", REQUEST_TIMEOUT).await?;
        Ok(response.windows)
    }

    async fn active_window(&self, endpoint: &AgentEndpoint) -> PortResult<ActiveWindowResponse> {
        self.get(endpoint, "/v1/windows/active", REQUEST_TIMEOUT)
            .await
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
        command: RemoteCommand,
    ) -> PortResult<()> {
        let _: omdesky_protocol::CommandResponse = self
            .post_json(endpoint, "/v1/commands", &CommandRequest { command })
            .await?;
        Ok(())
    }
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
