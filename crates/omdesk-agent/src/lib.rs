use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use omdesk_application::ports::{
    AccessStore, AgentClient, AgentEndpoint, CommandExecutor, MeshNetwork, RemoteOmarchy,
    SessionKeybindConfig, SessionKeybindInstaller, StreamHost,
};
use omdesk_core::{DomainError, RemoteCommand, SessionRole, Window, WorkspaceId, WorkspaceTarget};
use omdesk_protocol::{
    ActiveWindowResponse, CommandRequest, CommandResponse, DisplaysResponse, ErrorEnvelope,
    FocusResponse, FocusWorkspaceRequest, HealthResponse, NodeInfoResponse, PROTOCOL_V1,
    ProtocolError, SunshinePairRequest, SunshinePairResponse, SunshineStatusResponse,
    WindowsResponse, WorkspacesResponse,
};
use serde::Deserialize;
use serde_json::Map;
use std::{
    net::SocketAddr,
    sync::{Arc, RwLock},
};

#[derive(Clone)]
pub struct SessionConfig {
    pub role: SessionRole,
    pub controller: Option<AgentEndpoint>,
}

#[derive(Clone)]
pub struct AgentState {
    pub node: NodeInfoResponse,
    pub desktop: Arc<dyn RemoteOmarchy>,
    pub sunshine: Arc<dyn StreamHost>,
    pub mesh: Arc<dyn MeshNetwork>,
    pub access: Arc<dyn AccessStore>,
    pub commands: Arc<dyn CommandExecutor>,
    pub keybinds: Arc<dyn SessionKeybindInstaller>,
    pub agent_client: Arc<dyn AgentClient>,
    pub session: Arc<RwLock<Option<SessionConfig>>>,
}

pub fn router(state: AgentState) -> Router {
    Router::new()
        .route("/v1/health", get(health))
        .route("/v1/node", get(node))
        .route("/v1/displays", get(displays))
        .route("/v1/workspaces", get(workspaces))
        .route("/v1/workspaces/focus", post(focus_workspace))
        .route("/v1/windows", get(windows))
        .route("/v1/windows/active", get(active_window))
        .route("/v1/windows/{window_id}/focus", post(focus_window))
        .route("/v1/commands", post(run_command))
        .route("/v1/sunshine", get(sunshine_status))
        .route("/v1/sunshine/pair", post(sunshine_pair))
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        protocol: PROTOCOL_V1,
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
    })
}

async fn node(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<NodeInfoResponse>, ApiError> {
    authorize(&state, source).await?;
    Ok(Json(state.node.clone()))
}

async fn displays(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<DisplaysResponse>, ApiError> {
    authorize(&state, source).await?;
    Ok(Json(DisplaysResponse {
        displays: state.desktop.displays().await?,
    }))
}

async fn workspaces(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<WorkspacesResponse>, ApiError> {
    authorize(&state, source).await?;
    Ok(Json(WorkspacesResponse {
        workspaces: state.desktop.workspaces().await?,
    }))
}

#[derive(Debug, Default, Deserialize)]
struct WindowFilter {
    workspace: Option<i64>,
    app_id: Option<String>,
}

async fn windows(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Query(filter): Query<WindowFilter>,
) -> Result<Json<WindowsResponse>, ApiError> {
    authorize(&state, source).await?;

    let windows = state
        .desktop
        .windows()
        .await?
        .into_iter()
        .filter(|window| filter_matches(window, &filter))
        .collect();

    Ok(Json(WindowsResponse { windows }))
}

fn filter_matches(window: &Window, filter: &WindowFilter) -> bool {
    let workspace_ok = filter
        .workspace
        .is_none_or(|id| window.workspace == WorkspaceId(id));
    let app_ok = filter.app_id.as_deref().is_none_or(|needle| {
        window.app_id.as_deref() == Some(needle) || window.class.as_deref() == Some(needle)
    });
    workspace_ok && app_ok
}

async fn active_window(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<ActiveWindowResponse>, ApiError> {
    authorize(&state, source).await?;
    Ok(Json(ActiveWindowResponse {
        window: state.desktop.active_window().await?,
    }))
}

async fn focus_workspace(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Json(request): Json<FocusWorkspaceRequest>,
) -> Result<Json<FocusResponse>, ApiError> {
    authorize(&state, source).await?;
    validate_workspace_target(&request.target)?;
    state.desktop.focus_workspace(request.target).await?;
    Ok(Json(FocusResponse { focused: true }))
}

async fn focus_window(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Path(window_id): Path<String>,
) -> Result<Json<FocusResponse>, ApiError> {
    authorize(&state, source).await?;
    state.desktop.focus_window(&window_id).await?;
    Ok(Json(FocusResponse { focused: true }))
}

async fn run_command(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Json(request): Json<CommandRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    authorize(&state, source).await?;

    let command = request.command;
    command.validate().map_err(domain_error)?;

    if command.is_session_control() {
        apply_session_command(&state, command).await?;
        return Ok(Json(CommandResponse { executed: true }));
    }

    match current_role(&state) {
        Some(SessionRole::Remote) => {
            if command.controller_exclusive() {
                return Err(ApiError::new(
                    StatusCode::BAD_REQUEST,
                    "COMMAND_NOT_ALLOWED_ON_REMOTE",
                    "command is exclusive to the controller and is not relayed",
                    false,
                ));
            }

            let controller = current_controller(&state).ok_or_else(|| {
                ApiError::new(
                    StatusCode::CONFLICT,
                    "NO_CONTROLLER",
                    "remote session has no controller endpoint to relay to",
                    false,
                )
            })?;

            state
                .agent_client
                .send_command(&controller, command)
                .await?;
            Ok(Json(CommandResponse { executed: true }))
        }
        _ => {
            state.commands.execute(command).await?;
            Ok(Json(CommandResponse { executed: true }))
        }
    }
}

async fn apply_session_command(state: &AgentState, command: RemoteCommand) -> Result<(), ApiError> {
    match command {
        RemoteCommand::AttachSession { role, controller } => {
            let controller = controller.map(|endpoint| AgentEndpoint {
                address: endpoint.address,
                port: endpoint.port,
            });

            if let Ok(mut guard) = state.session.write() {
                *guard = Some(SessionConfig {
                    role,
                    controller: controller.clone(),
                });
            }

            state
                .keybinds
                .install(SessionKeybindConfig { role, controller })
                .await?;

            Ok(())
        }
        RemoteCommand::DetachSession => {
            if let Ok(mut guard) = state.session.write() {
                *guard = None;
            }

            state.keybinds.clear().await?;

            Ok(())
        }
        _ => Ok(()),
    }
}

fn current_role(state: &AgentState) -> Option<SessionRole> {
    state
        .session
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().map(|config| config.role))
}

fn current_controller(state: &AgentState) -> Option<AgentEndpoint> {
    state
        .session
        .read()
        .ok()
        .and_then(|guard| guard.as_ref().and_then(|config| config.controller.clone()))
}

async fn sunshine_status(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<SunshineStatusResponse>, ApiError> {
    authorize(&state, source).await?;
    Ok(Json(state.sunshine.status().await?))
}

async fn sunshine_pair(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Json(request): Json<SunshinePairRequest>,
) -> Result<Json<SunshinePairResponse>, ApiError> {
    authorize(&state, source).await?;
    state.sunshine.submit_pairing_pin(request).await?;
    Ok(Json(SunshinePairResponse { paired: true }))
}

fn domain_error(error: DomainError) -> ApiError {
    let code = match error {
        DomainError::InvalidKeyChord => "INVALID_KEY_CHORD",
        DomainError::InvalidWindowSelector => "INVALID_WINDOW_SELECTOR",
        DomainError::InvalidSessionEndpoint => "INVALID_SESSION_ENDPOINT",
        _ => "INVALID_COMMAND",
    };

    ApiError::new(StatusCode::BAD_REQUEST, code, error.to_string(), false)
}

fn validate_workspace_target(target: &WorkspaceTarget) -> Result<(), ApiError> {
    if let WorkspaceTarget::Name(name) = target
        && (name.is_empty() || name.contains(char::is_whitespace))
    {
        return Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "WORKSPACE_NOT_FOUND",
            "workspace name is not a safe selector",
            false,
        ));
    }
    Ok(())
}

pub async fn authorize(state: &AgentState, source: SocketAddr) -> Result<(), ApiError> {
    let identity = state
        .mesh
        .identify_source(source.ip())
        .await
        .map_err(|error| {
            ApiError::new(
                StatusCode::UNAUTHORIZED,
                "UNAUTHORIZED",
                error.message,
                false,
            )
        })?
        .ok_or_else(unauthorized)?;

    if identity.tailnet_node_id.is_empty() {
        return Err(unauthorized());
    }

    let allowlist = state.access.list().await.map_err(|error| {
        ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "ACCESS_STORE_FAILED",
            error.message,
            false,
        )
    })?;

    if !allowlist.is_empty()
        && !allowlist
            .iter()
            .any(|entry| entry.tailnet_node_id == identity.tailnet_node_id)
    {
        return Err(unauthorized());
    }

    Ok(())
}

fn unauthorized() -> ApiError {
    ApiError::new(
        StatusCode::UNAUTHORIZED,
        "UNAUTHORIZED",
        "caller is not an authorized Tailscale identity",
        false,
    )
}

pub struct ApiError {
    status: StatusCode,
    envelope: ErrorEnvelope,
}

impl ApiError {
    pub fn new(
        status: StatusCode,
        code: impl Into<String>,
        message: impl Into<String>,
        retryable: bool,
    ) -> Self {
        Self {
            status,
            envelope: ErrorEnvelope {
                error: ProtocolError {
                    code: code.into(),
                    message: message.into(),
                    retryable,
                    details: Map::new(),
                },
            },
        }
    }
}

impl From<omdesk_application::ports::PortError> for ApiError {
    fn from(error: omdesk_application::ports::PortError) -> Self {
        let status = match error.code {
            "HYPRLAND_UNAVAILABLE" => StatusCode::SERVICE_UNAVAILABLE,
            "WORKSPACE_NOT_FOUND" | "WINDOW_NOT_FOUND" | "DISPLAY_NOT_FOUND" => {
                StatusCode::NOT_FOUND
            }
            "SUNSHINE_NOT_INSTALLED" | "SUNSHINE_NOT_RUNNING" | "SUNSHINE_API_UNAVAILABLE" => {
                StatusCode::SERVICE_UNAVAILABLE
            }
            "SUNSHINE_PAIRING_FAILED" => StatusCode::BAD_GATEWAY,
            _ => StatusCode::INTERNAL_SERVER_ERROR,
        };
        ApiError::new(status, error.code, error.message, error.retryable)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.status, Json(self.envelope)).into_response()
    }
}
