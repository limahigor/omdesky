#![forbid(unsafe_code)]

pub mod authorize;
pub mod error;
pub mod pairing;
pub mod replay;
pub mod session;

use axum::{
    Json, Router,
    extract::{ConnectInfo, DefaultBodyLimit, Path, Query, Request, State},
    http::{HeaderName, HeaderValue},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use omdesky_application::display::FollowFocusRouter;
use omdesky_application::ports::{
    AgentClient, AgentEndpoint, CommandExecutor, Notification, NotificationService, RemoteOmarchy,
    StreamHost,
};
use omdesky_application::services::MOONLIGHT_WINDOW_CLASS;
use omdesky_core::{
    ControlCapability, RemoteCommand, SessionClaim, SessionEndpoint, SessionRole, Window,
    WindowSelector, WorkspaceId, WorkspaceTarget,
};
use omdesky_platform::display::{
    HyprlandDisplayTopology, MoonlightDisplayController, spawn_focus_signals,
};
use omdesky_protocol::{
    ActiveWindowResponse, CapabilitiesResponse, CommandRequest, CommandResponse, DisplaysResponse,
    ErrorCode, FocusResponse, FocusWorkspaceRequest, HealthResponse, NodeInfoResponse, PROTOCOL,
    RELEASE, RELEASE_HEADER, SunshinePairChallengeResponse, SunshinePairRequest,
    SunshinePairResponse, SunshineStatusResponse, WindowsResponse, WorkspacesResponse,
    is_compatible_release,
};
use serde::Deserialize;
use std::{
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{sync::mpsc, task::JoinHandle};

use crate::{
    authorize::{AuthorizedPeer, Authorizer},
    error::{ApiError, ApiJson},
    pairing::PairingChallenges,
    replay::ReplayGuard,
    session::{SessionCoordinator, SessionOutcome},
};

const FOLLOW_FOCUS_DEBOUNCE: Duration = Duration::from_millis(50);
const MAX_REQUEST_BODY_BYTES: usize = 64 * 1024;

#[derive(Clone)]
pub struct AgentState {
    pub node: NodeInfoResponse,
    pub desktop: Arc<dyn RemoteOmarchy>,
    pub sunshine: Arc<dyn StreamHost>,
    pub commands: Arc<dyn CommandExecutor>,
    pub agent_client: Arc<dyn AgentClient>,
    pub notifications: Arc<dyn NotificationService>,
    pub authorizer: Arc<Authorizer>,
    pub sessions: Arc<SessionCoordinator>,
    pub challenges: Arc<PairingChallenges>,
    pub replay: Arc<ReplayGuard>,
    pub agent_port: u16,
}

pub struct FollowFocusSession {
    source: JoinHandle<()>,
    router: JoinHandle<()>,
}

impl FollowFocusSession {
    fn stop(self) {
        self.source.abort();
        self.router.abort();
    }
}

#[derive(Clone)]
pub struct FollowFocusSupervisor {
    agent_client: Arc<dyn AgentClient>,
    desktop: Arc<dyn RemoteOmarchy>,
    notifications: Arc<dyn NotificationService>,
    running: Arc<Mutex<Option<FollowFocusSession>>>,
}

impl FollowFocusSupervisor {
    pub fn new(
        agent_client: Arc<dyn AgentClient>,
        desktop: Arc<dyn RemoteOmarchy>,
        notifications: Arc<dyn NotificationService>,
    ) -> Self {
        Self {
            agent_client,
            desktop,
            notifications,
            running: Arc::new(Mutex::new(None)),
        }
    }

    pub fn set_controller(&self, controller: Option<AgentEndpoint>) {
        let replacement = controller.map(|controller| {
            let display_controller = Arc::new(MoonlightDisplayController::new(
                self.agent_client.clone(),
                controller,
                self.desktop.clone(),
            ));
            let topology = Arc::new(HyprlandDisplayTopology::new(self.desktop.clone()));

            let (sender, receiver) = mpsc::channel(32);
            let source = spawn_focus_signals(sender);
            let router = FollowFocusRouter::new(
                display_controller,
                topology,
                self.notifications.clone(),
                FOLLOW_FOCUS_DEBOUNCE,
            );

            FollowFocusSession {
                source,
                router: tokio::spawn(router.run(receiver)),
            }
        });

        let Ok(mut running) = self.running.lock() else {
            return;
        };

        let previous = match replacement {
            Some(session) => running.replace(session),
            None => running.take(),
        };

        if let Some(previous) = previous {
            previous.stop();
        }
    }
}

pub fn router(state: AgentState) -> Router {
    let controlled = Router::new()
        .route("/v1/node", get(node))
        .route("/v1/capabilities", get(capabilities))
        .route("/v1/displays", get(displays))
        .route("/v1/workspaces", get(workspaces))
        .route("/v1/workspaces/focus", post(focus_workspace))
        .route("/v1/windows", get(windows))
        .route("/v1/windows/active", get(active_window))
        .route("/v1/windows/{window_id}/focus", post(focus_window))
        .route("/v1/commands", post(run_command))
        .route("/v1/sunshine", get(sunshine_status))
        .route("/v1/sunshine/pair/challenge", post(sunshine_pair_challenge))
        .route("/v1/sunshine/pair", post(sunshine_pair))
        .route_layer(middleware::from_fn(require_compatible_release));

    Router::new()
        .route("/v1/health", get(health))
        .merge(controlled)
        .layer(middleware::map_response(advertise_release))
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

async fn require_compatible_release(request: Request, next: Next) -> Response {
    let compatible = request
        .headers()
        .get(RELEASE_HEADER)
        .and_then(|value| value.to_str().ok())
        .is_some_and(is_compatible_release);

    if !compatible {
        tracing::debug!(path = %request.uri().path(), "request.release_incompatible");

        return ApiError::new(
            ErrorCode::VersionIncompatible,
            format!("this device runs Omdesky {RELEASE}; both computers must run the same release"),
        )
        .into_response();
    }

    next.run(request).await
}

async fn advertise_release(mut response: Response) -> Response {
    response.headers_mut().insert(
        HeaderName::from_static(RELEASE_HEADER),
        HeaderValue::from_static(RELEASE),
    );

    response
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".to_owned(),
        protocol: PROTOCOL,
        agent_version: RELEASE.to_owned(),
    })
}

async fn node(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<NodeInfoResponse>, ApiError> {
    authorize(&state, source, ControlCapability::ReadMetadata).await?;
    Ok(Json(state.node.clone()))
}

async fn capabilities(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<CapabilitiesResponse>, ApiError> {
    let peer = state
        .authorizer
        .authenticate(source.ip())
        .await
        .map_err(ApiError::from)?;

    Ok(Json(CapabilitiesResponse {
        capabilities: peer.capabilities,
    }))
}

async fn displays(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<DisplaysResponse>, ApiError> {
    authorize(&state, source, ControlCapability::ReadMetadata).await?;
    Ok(Json(DisplaysResponse {
        displays: state.desktop.displays().await?,
    }))
}

async fn workspaces(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<WorkspacesResponse>, ApiError> {
    authorize(&state, source, ControlCapability::ReadMetadata).await?;
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
    authorize(&state, source, ControlCapability::ReadMetadata).await?;

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
    authorize(&state, source, ControlCapability::ReadMetadata).await?;
    Ok(Json(ActiveWindowResponse {
        window: state.desktop.active_window().await?,
    }))
}

async fn focus_workspace(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    ApiJson(request): ApiJson<FocusWorkspaceRequest>,
) -> Result<Json<FocusResponse>, ApiError> {
    authorize(&state, source, ControlCapability::FocusWorkspace).await?;
    validate_workspace_target(&request.target)?;
    state.desktop.focus_workspace(request.target).await?;
    Ok(Json(FocusResponse { focused: true }))
}

async fn focus_window(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    Path(window_id): Path<String>,
) -> Result<Json<FocusResponse>, ApiError> {
    authorize(&state, source, ControlCapability::FocusWorkspace).await?;
    state.desktop.focus_window(&window_id).await?;
    Ok(Json(FocusResponse { focused: true }))
}

async fn run_command(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    ApiJson(request): ApiJson<CommandRequest>,
) -> Result<Json<CommandResponse>, ApiError> {
    let command = request.command;

    let peer = authorize(&state, source, command.required_capability()).await?;

    command.validate().map_err(ApiError::from)?;

    state
        .replay
        .admit(request.request_id, request.issued_at)
        .await
        .map_err(ApiError::from)?;

    if command.is_session_control() {
        let response =
            apply_session_command(&state, &peer, source, command, request.session).await?;

        return Ok(Json(response));
    }

    let session = state.sessions.snapshot().await;

    match session.as_ref().map(|session| session.role) {
        Some(SessionRole::Remote) => {
            let controller = session
                .and_then(|session| session.controller)
                .ok_or_else(|| {
                    ApiError::new(
                        ErrorCode::NoController,
                        "remote session has no controller endpoint to relay to",
                    )
                })?;

            state
                .agent_client
                .send_command(&controller, CommandRequest::new(command))
                .await?;

            Ok(Json(CommandResponse::executed()))
        }
        _ => {
            if let RemoteCommand::SwitchStreamDisplay { display } = command {
                let target = display;
                let remote_endpoint = AgentEndpoint {
                    address: source.ip(),
                    port: state.agent_port,
                };

                tracing::info!(
                    display_id = %target,
                    port = remote_endpoint.port,
                    "controller.switch_display.received"
                );

                send_debug_notification(&state, format!("Display switch requested for {target}"))
                    .await;

                let displays = state.agent_client.displays(&remote_endpoint).await?;

                let shortcut =
                    omdesky_platform::display::resolve_display_switch_shortcut(&displays, &target)?;

                let outcome = state.commands.execute(shortcut).await;

                let body = match &outcome {
                    Ok(()) => format!("Display switch sent for {target}"),
                    Err(_) => format!("Display switch failed for {target}"),
                };
                send_debug_notification(&state, body).await;

                outcome?;

                tracing::info!("controller.switch_display.dispatched");

                return Ok(Json(CommandResponse::executed()));
            }

            let notification = controller_command_notification(&command);

            state.commands.execute(command).await?;

            if let Some(notification) = notification {
                let _ = state.notifications.send(notification).await;
            }

            Ok(Json(CommandResponse::executed()))
        }
    }
}

#[cfg(debug_assertions)]
async fn send_debug_notification(state: &AgentState, body: String) {
    if let Err(error) = state
        .notifications
        .send(Notification {
            summary: "Omdesky debug".to_owned(),
            body,
        })
        .await
    {
        tracing::debug!(code = error.code, detail = %error.message, "notification.debug_failed");
    }
}

#[cfg(not(debug_assertions))]
async fn send_debug_notification(_state: &AgentState, _body: String) {}

fn controller_command_notification(command: &RemoteCommand) -> Option<Notification> {
    let body = match command {
        RemoteCommand::SendShortcut { .. } => "Remote shortcut capture changed",
        RemoteCommand::CloseWindow { .. } => "The remote session was closed",
        RemoteCommand::SwitchStreamDisplay { .. }
        | RemoteCommand::AttachSession { .. }
        | RemoteCommand::DetachSession
        | RemoteCommand::RenewSession => return None,
    };

    Some(Notification {
        summary: "Omdesky".to_owned(),
        body: body.to_owned(),
    })
}

async fn apply_session_command(
    state: &AgentState,
    peer: &AuthorizedPeer,
    source: SocketAddr,
    command: RemoteCommand,
    claim: Option<SessionClaim>,
) -> Result<CommandResponse, ApiError> {
    match command {
        RemoteCommand::AttachSession {
            role,
            controller,
            window,
        } => {
            let controller = controller
                .map(|endpoint| callback_endpoint(endpoint, source))
                .transpose()?;
            let window =
                window.unwrap_or_else(|| WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned()));

            if let Some(controller) = &controller {
                verify_callback_access(state, controller).await?;
            }

            let grant = state
                .sessions
                .attach(&peer.tailnet_node_id, role, controller, window)
                .await
                .map_err(ApiError::from)?;

            tracing::info!(
                generation = grant.generation,
                role = ?role,
                "session.attached"
            );

            Ok(CommandResponse::granted(grant))
        }
        RemoteCommand::DetachSession => {
            let claim = claim.ok_or_else(missing_session_claim)?;

            let outcome = state
                .sessions
                .detach(&peer.tailnet_node_id, claim)
                .await
                .map_err(ApiError::from)?;

            Ok(match outcome {
                SessionOutcome::Applied => CommandResponse::executed(),
                SessionOutcome::Ignored => CommandResponse::ignored(),
            })
        }
        RemoteCommand::RenewSession => {
            let claim = claim.ok_or_else(missing_session_claim)?;

            let grant = state
                .sessions
                .renew(&peer.tailnet_node_id, claim)
                .await
                .map_err(ApiError::from)?;

            Ok(CommandResponse::granted(grant))
        }
        _ => Ok(CommandResponse::ignored()),
    }
}

async fn verify_callback_access(
    state: &AgentState,
    controller: &AgentEndpoint,
) -> Result<(), ApiError> {
    let granted = match state.agent_client.granted_capabilities(controller).await {
        Ok(granted) => granted,
        Err(error) => return Err(callback_probe_error(error)),
    };

    let missing = ControlCapability::CALLBACK
        .into_iter()
        .filter(|capability| !granted.contains(capability))
        .map(ControlCapability::as_str)
        .collect::<Vec<_>>();

    if missing.is_empty() {
        return Ok(());
    }

    tracing::warn!(missing = %missing.join(","), "session.callback_access_missing");

    Err(callback_access_missing())
}

fn callback_probe_error(error: omdesky_application::ports::PortError) -> ApiError {
    tracing::warn!(code = error.code, "session.callback_probe_failed");

    match error.code {
        "UNAUTHORIZED" | "CAPABILITY_DENIED" => callback_access_missing(),
        "VERSION_INCOMPATIBLE" => ApiError::new(
            ErrorCode::VersionIncompatible,
            "The controller runs a different Omdesky release.",
        ),
        _ => ApiError::new(
            ErrorCode::ControllerUnreachable,
            "This device could not reach the controller's agent.",
        ),
    }
}

fn callback_access_missing() -> ApiError {
    ApiError::new(
        ErrorCode::CallbackAccessMissing,
        "The controller does not allow this device to send shortcuts back.",
    )
}

fn callback_endpoint(
    endpoint: SessionEndpoint,
    source: SocketAddr,
) -> Result<AgentEndpoint, ApiError> {
    if endpoint.address != source.ip() {
        tracing::warn!("session.callback_endpoint_rejected");

        return Err(ApiError::new(
            ErrorCode::InvalidCommand,
            "The controller endpoint must be the address this request came from.",
        ));
    }

    Ok(AgentEndpoint {
        address: source.ip(),
        port: endpoint.port,
    })
}

fn missing_session_claim() -> ApiError {
    ApiError::new(
        ErrorCode::InvalidCommand,
        "This action requires the session it belongs to.",
    )
}

async fn sunshine_status(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<SunshineStatusResponse>, ApiError> {
    authorize(&state, source, ControlCapability::ReadMetadata).await?;
    Ok(Json(state.sunshine.status().await?))
}

async fn sunshine_pair_challenge(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
) -> Result<Json<SunshinePairChallengeResponse>, ApiError> {
    let peer = authorize(&state, source, ControlCapability::ApprovePairing).await?;

    let pairing_id = state
        .challenges
        .issue(&peer.tailnet_node_id)
        .await
        .map_err(ApiError::from)?;

    Ok(Json(SunshinePairChallengeResponse {
        pairing_id,
        expires_in_seconds: state.challenges.ttl_seconds(),
    }))
}

async fn sunshine_pair(
    State(state): State<AgentState>,
    ConnectInfo(source): ConnectInfo<SocketAddr>,
    ApiJson(request): ApiJson<SunshinePairRequest>,
) -> Result<Json<SunshinePairResponse>, ApiError> {
    let peer = authorize(&state, source, ControlCapability::ApprovePairing).await?;

    request.validate().map_err(|error| {
        tracing::debug!(field = ?error, "sunshine.pairing_request_invalid");

        ApiError::new(
            ErrorCode::InvalidCommand,
            "The pairing request is not valid.",
        )
    })?;

    state
        .challenges
        .consume(&peer.tailnet_node_id, &request.pairing_id)
        .await
        .map_err(ApiError::from)?;

    let client_name = request.client_name.clone();

    state
        .sunshine
        .submit_pairing_pin(&request.pin, &request.client_name)
        .await?;

    let _ = state
        .notifications
        .send(Notification {
            summary: "Omdesky".to_owned(),
            body: format!("Approved stream pairing for {client_name}"),
        })
        .await;

    tracing::info!("sunshine.pairing_approved");

    Ok(Json(SunshinePairResponse { paired: true }))
}

fn validate_workspace_target(target: &WorkspaceTarget) -> Result<(), ApiError> {
    if let WorkspaceTarget::Name(name) = target
        && (name.is_empty() || name.contains(char::is_whitespace))
    {
        return Err(ApiError::new(
            ErrorCode::WorkspaceNotFound,
            "The requested workspace name is not valid.",
        ));
    }

    Ok(())
}

pub async fn authorize(
    state: &AgentState,
    source: SocketAddr,
    required: ControlCapability,
) -> Result<AuthorizedPeer, ApiError> {
    state
        .authorizer
        .authorize(source.ip(), required)
        .await
        .map_err(ApiError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{authorize::AuthorizationError, replay::FreshnessError, session::SessionError};
    use omdesky_core::{KeyChord, KeyModifier, WindowSelector};
    use std::net::{IpAddr, Ipv4Addr};

    fn source() -> SocketAddr {
        SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7)), 51234)
    }

    #[test]
    fn test_controller_shortcut_notification_describes_capture_toggle() {
        let command = RemoteCommand::SendShortcut {
            chord: KeyChord::new(
                [KeyModifier::Ctrl, KeyModifier::Alt, KeyModifier::Shift],
                "Z",
            )
            .expect("valid shortcut"),
            window: WindowSelector::ActiveWindow,
        };

        assert_eq!(
            controller_command_notification(&command),
            Some(Notification {
                summary: "Omdesky".to_owned(),
                body: "Remote shortcut capture changed".to_owned(),
            })
        );
    }

    #[test]
    fn test_controller_close_notification_describes_session_close() {
        let command = RemoteCommand::CloseWindow {
            window: WindowSelector::ActiveWindow,
        };

        assert_eq!(
            controller_command_notification(&command),
            Some(Notification {
                summary: "Omdesky".to_owned(),
                body: "The remote session was closed".to_owned(),
            })
        );
    }

    #[test]
    fn test_session_commands_have_no_controller_action_notification() {
        assert_eq!(
            controller_command_notification(&RemoteCommand::DetachSession),
            None
        );
    }

    #[test]
    fn test_display_switch_command_has_no_controller_notification() {
        let command = RemoteCommand::SwitchStreamDisplay {
            display: omdesky_core::DisplayId::from("DP-2"),
        };
        assert_eq!(controller_command_notification(&command), None);
    }

    #[test]
    fn test_callback_endpoint_must_match_the_authenticated_peer() {
        let matching = callback_endpoint(
            SessionEndpoint {
                address: source().ip(),
                port: 48155,
            },
            source(),
        )
        .expect("the caller's own address is accepted");
        assert_eq!(matching.address, source().ip());
        assert_eq!(matching.port, 48155);

        let loopback = callback_endpoint(
            SessionEndpoint {
                address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                port: 8080,
            },
            source(),
        )
        .expect_err("a foreign callback address is rejected");
        assert_eq!(loopback.code(), ErrorCode::InvalidCommand);

        let other_peer = callback_endpoint(
            SessionEndpoint {
                address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9)),
                port: 48155,
            },
            source(),
        )
        .expect_err("another peer's address is rejected");
        assert_eq!(other_peer.code(), ErrorCode::InvalidCommand);
    }

    fn status(error: impl Into<ApiError>) -> u16 {
        error.into().code().http_status()
    }

    #[test]
    fn test_authorization_failures_map_to_distinct_statuses() {
        assert_eq!(status(AuthorizationError::Unauthorized), 401);
        assert_eq!(status(AuthorizationError::Forbidden), 403);
        assert_eq!(status(AuthorizationError::RateLimited), 429);
        assert_eq!(status(AuthorizationError::Unavailable), 503);
    }

    #[test]
    fn test_session_failures_map_to_distinct_statuses() {
        assert_eq!(status(SessionError::OwnedByAnotherController), 409);
        assert_eq!(status(SessionError::NotOwner), 403);
        assert_eq!(status(SessionError::NotFound), 404);
    }

    #[test]
    fn test_stale_and_replayed_requests_are_distinguished() {
        assert_eq!(status(FreshnessError::Replayed), 409);
        assert_eq!(status(FreshnessError::Stale), 400);
    }
}
