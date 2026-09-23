use async_trait::async_trait;
use axum::{
    body::{Body, to_bytes},
    extract::ConnectInfo,
    http::{Request, StatusCode},
};
use omdesky_agent::{
    AgentState,
    authorize::Authorizer,
    pairing::PairingChallenges,
    replay::ReplayGuard,
    router,
    session::{ActiveSession, SessionCoordinator, SessionEffects},
};
use omdesky_application::ports::{
    AccessStore, AgentClient, AgentEndpoint, AllowedController, CommandExecutor, ConnectionInfo,
    MeshNetwork, MeshNodeIdentity, Notification, NotificationService, PortError, PortResult,
    RemoteOmarchy, StreamHost,
};
use omdesky_core::{
    ControlCapability, Display, MeshPeer, RemoteCommand, SessionEndpoint, SessionGrant,
    SessionRole, Window, WindowSelector, Workspace, WorkspaceTarget,
};
use omdesky_protocol::{
    ActiveWindowResponse, CommandRequest, CommandResponse, ErrorEnvelope, HealthResponse,
    NodeInfoResponse, RELEASE, RELEASE_HEADER, ReleaseLine, SunshinePairChallengeResponse,
    SunshinePairRequest, SunshineStatusResponse,
};
use serde::de::DeserializeOwned;
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::Arc,
    time::Duration,
};
use time::OffsetDateTime;
use tower::ServiceExt;

const PEER: &str = "nPEER01";

fn peer_source() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7)), 51234)
}

fn other_source() -> SocketAddr {
    SocketAddr::new(IpAddr::V4(Ipv4Addr::new(100, 64, 0, 9)), 51234)
}

struct StaticMesh;

#[async_trait]
impl MeshNetwork for StaticMesh {
    async fn local_node(&self) -> PortResult<MeshNodeIdentity> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn peers(&self) -> PortResult<Vec<MeshPeer>> {
        Ok(Vec::new())
    }

    async fn connection_info(&self, _id: &str) -> PortResult<ConnectionInfo> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn identify_source(&self, source: IpAddr) -> PortResult<Option<MeshNodeIdentity>> {
        let tailnet_node_id = match source {
            address if address == peer_source().ip() => PEER,
            address if address == other_source().ip() => "nOTHER1",
            _ => return Ok(None),
        };

        Ok(Some(MeshNodeIdentity {
            tailnet_node_id: tailnet_node_id.to_owned(),
            user: None,
            hostname: None,
            addresses: Vec::new(),
        }))
    }
}

struct StaticAccess {
    entries: Vec<AllowedController>,
}

#[async_trait]
impl AccessStore for StaticAccess {
    async fn list(&self) -> PortResult<Vec<AllowedController>> {
        Ok(self.entries.clone())
    }

    async fn allow(&self, _controller: AllowedController) -> PortResult<()> {
        Ok(())
    }

    async fn revoke(&self, _tailnet_node_id: &str) -> PortResult<()> {
        Ok(())
    }

    async fn is_allowed(&self, _tailnet_node_id: &str) -> PortResult<bool> {
        Ok(false)
    }
}

struct EmptyDesktop;

#[async_trait]
impl RemoteOmarchy for EmptyDesktop {
    async fn displays(&self) -> PortResult<Vec<Display>> {
        Ok(Vec::new())
    }

    async fn workspaces(&self) -> PortResult<Vec<Workspace>> {
        Ok(Vec::new())
    }

    async fn windows(&self) -> PortResult<Vec<Window>> {
        Ok(Vec::new())
    }

    async fn active_window(&self) -> PortResult<Option<Window>> {
        Ok(None)
    }

    async fn focus_workspace(&self, _target: WorkspaceTarget) -> PortResult<()> {
        Ok(())
    }

    async fn focus_window(&self, _id: &str) -> PortResult<()> {
        Ok(())
    }
}

struct UnusedStreamHost;

#[async_trait]
impl StreamHost for UnusedStreamHost {
    async fn readiness(&self) -> PortResult<omdesky_application::ports::HostReadiness> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn status(&self) -> PortResult<SunshineStatusResponse> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn displays(&self) -> PortResult<Vec<Display>> {
        Ok(Vec::new())
    }

    async fn submit_pairing_pin(&self, _request: SunshinePairRequest) -> PortResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct RecordingCommands {
    executed: std::sync::Mutex<Vec<RemoteCommand>>,
}

#[async_trait]
impl CommandExecutor for RecordingCommands {
    async fn execute(&self, command: RemoteCommand) -> PortResult<()> {
        self.executed.lock().expect("lock").push(command);

        Ok(())
    }
}

struct UnusedAgentClient;

#[async_trait]
impl AgentClient for UnusedAgentClient {
    async fn health(&self, _endpoint: &AgentEndpoint) -> PortResult<HealthResponse> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn node_info(&self, _endpoint: &AgentEndpoint) -> PortResult<NodeInfoResponse> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn displays(&self, _endpoint: &AgentEndpoint) -> PortResult<Vec<Display>> {
        Ok(Vec::new())
    }

    async fn workspaces(&self, _endpoint: &AgentEndpoint) -> PortResult<Vec<Workspace>> {
        Ok(Vec::new())
    }

    async fn windows(&self, _endpoint: &AgentEndpoint) -> PortResult<Vec<Window>> {
        Ok(Vec::new())
    }

    async fn active_window(&self, _endpoint: &AgentEndpoint) -> PortResult<ActiveWindowResponse> {
        Ok(ActiveWindowResponse { window: None })
    }

    async fn focus_workspace(
        &self,
        _endpoint: &AgentEndpoint,
        _target: WorkspaceTarget,
    ) -> PortResult<()> {
        Ok(())
    }

    async fn focus_window(&self, _endpoint: &AgentEndpoint, _window: &str) -> PortResult<()> {
        Ok(())
    }

    async fn sunshine_status(
        &self,
        _endpoint: &AgentEndpoint,
    ) -> PortResult<SunshineStatusResponse> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn sunshine_pair_challenge(
        &self,
        _endpoint: &AgentEndpoint,
    ) -> PortResult<SunshinePairChallengeResponse> {
        Err(PortError::new("UNUSED", "unused", false))
    }

    async fn sunshine_pair(
        &self,
        _endpoint: &AgentEndpoint,
        _request: SunshinePairRequest,
    ) -> PortResult<()> {
        Ok(())
    }

    async fn send_command(
        &self,
        _endpoint: &AgentEndpoint,
        _request: CommandRequest,
    ) -> PortResult<CommandResponse> {
        Ok(CommandResponse::executed())
    }
}

struct SilentNotifications;

#[async_trait]
impl NotificationService for SilentNotifications {
    async fn send(&self, _notification: Notification) -> PortResult<()> {
        Ok(())
    }
}

#[derive(Default)]
struct CountingEffects {
    installs: std::sync::Mutex<usize>,
    clears: std::sync::Mutex<usize>,
}

#[async_trait]
impl SessionEffects for CountingEffects {
    async fn install(&self, _session: &ActiveSession) -> PortResult<()> {
        *self.installs.lock().expect("lock") += 1;

        Ok(())
    }

    async fn clear(&self) -> PortResult<()> {
        *self.clears.lock().expect("lock") += 1;

        Ok(())
    }
}

fn state(entries: Vec<AllowedController>) -> AgentState {
    AgentState {
        node: NodeInfoResponse {
            node_id: "local".to_owned(),
            hostname: "local".to_owned(),
            omarchy_version: "4.0.0".to_owned(),
            agent_version: "0.1.1".to_owned(),
            protocol_versions: vec![1],
            capabilities: Vec::new(),
        },
        desktop: Arc::new(EmptyDesktop),
        sunshine: Arc::new(UnusedStreamHost),
        commands: Arc::new(RecordingCommands::default()),
        agent_client: Arc::new(UnusedAgentClient),
        notifications: Arc::new(SilentNotifications),
        authorizer: Arc::new(Authorizer::new(
            Arc::new(StaticMesh),
            Arc::new(StaticAccess { entries }),
        )),
        sessions: Arc::new(SessionCoordinator::new(
            Arc::new(CountingEffects::default()),
            Duration::from_secs(30),
        )),
        challenges: Arc::new(PairingChallenges::default()),
        replay: Arc::new(ReplayGuard::default()),
        agent_port: 48155,
    }
}

fn entry(id: &str, capabilities: &[ControlCapability]) -> AllowedController {
    AllowedController::new(
        id,
        None,
        OffsetDateTime::UNIX_EPOCH,
        capabilities.iter().copied(),
    )
}

async fn call(
    state: &AgentState,
    source: SocketAddr,
    request: Request<Body>,
) -> (StatusCode, Vec<u8>) {
    let mut request = request;
    request.extensions_mut().insert(ConnectInfo(source));

    let response = router(state.clone())
        .oneshot(request)
        .await
        .expect("the router responds");
    let status = response.status();
    let body = to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("the body is readable")
        .to_vec();

    (status, body)
}

fn get(path: &str) -> Request<Body> {
    Request::builder()
        .uri(path)
        .header(RELEASE_HEADER, RELEASE)
        .body(Body::empty())
        .expect("valid request")
}

fn post_json<T: serde::Serialize>(path: &str, body: &T) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(path)
        .header(RELEASE_HEADER, RELEASE)
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("valid request")
}

fn decode<T: DeserializeOwned>(body: &[u8]) -> T {
    serde_json::from_slice(body).expect("decodable response")
}

async fn attach(state: &AgentState, source: SocketAddr) -> SessionGrant {
    let (status, body) = call(
        state,
        source,
        post_json(
            "/v1/commands",
            &CommandRequest::new(RemoteCommand::AttachSession {
                role: SessionRole::Remote,
                window: Some(WindowSelector::Address("0x55aa".to_owned())),
                controller: Some(SessionEndpoint {
                    address: source.ip(),
                    port: 48155,
                }),
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);

    decode::<CommandResponse>(&body)
        .session
        .expect("the agent issues a session grant")
}

#[tokio::test]
async fn test_health_stays_reachable_without_authorization() {
    let state = state(Vec::new());

    let (status, body) = call(&state, peer_source(), get("/v1/health")).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(decode::<HealthResponse>(&body).protocol, 1);
}

fn get_with_release(path: &str, release: Option<&str>) -> Request<Body> {
    let builder = Request::builder().uri(path);

    let builder = match release {
        Some(release) => builder.header(RELEASE_HEADER, release),
        None => builder,
    };

    builder.body(Body::empty()).expect("valid request")
}

fn other_minor_release() -> String {
    let current = ReleaseLine::current().expect("current release parses");

    format!("{}.{}.0", current.major, current.minor + 1)
}

async fn call_with_headers(
    state: &AgentState,
    source: SocketAddr,
    request: Request<Body>,
) -> (StatusCode, Option<String>) {
    let mut request = request;
    request.extensions_mut().insert(ConnectInfo(source));

    let response = router(state.clone())
        .oneshot(request)
        .await
        .expect("the router responds");

    let release = response
        .headers()
        .get(RELEASE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);

    (response.status(), release)
}

#[tokio::test]
async fn test_health_answers_a_controller_of_any_release() {
    let state = state(Vec::new());

    let (status, release) =
        call_with_headers(&state, peer_source(), get_with_release("/v1/health", None)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(release.as_deref(), Some(RELEASE));
}

#[tokio::test]
async fn test_a_controller_without_a_release_is_refused_before_authorization() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let (status, body) = call(&state, peer_source(), get_with_release("/v1/node", None)).await;

    assert_eq!(status, StatusCode::CONFLICT);
    assert_eq!(
        decode::<ErrorEnvelope>(&body).error.code,
        "VERSION_INCOMPATIBLE"
    );
}

#[tokio::test]
async fn test_a_controller_from_another_release_line_is_refused() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let other = other_minor_release();

    for release in [other.as_str(), "999.0.0", "not-a-version"] {
        let (status, _) = call(
            &state,
            peer_source(),
            get_with_release("/v1/node", Some(release)),
        )
        .await;

        assert_eq!(status, StatusCode::CONFLICT, "{release} must be refused");
    }
}

#[tokio::test]
async fn test_every_response_advertises_the_agent_release() {
    let state = state(Vec::new());

    let (refused, refused_release) =
        call_with_headers(&state, peer_source(), get_with_release("/v1/node", None)).await;
    let (unauthorized, unauthorized_release) =
        call_with_headers(&state, peer_source(), get("/v1/node")).await;

    assert_eq!(refused, StatusCode::CONFLICT);
    assert_eq!(refused_release.as_deref(), Some(RELEASE));
    assert_eq!(unauthorized, StatusCode::UNAUTHORIZED);
    assert_eq!(unauthorized_release.as_deref(), Some(RELEASE));
}

#[tokio::test]
async fn test_an_empty_allowlist_denies_every_protected_route() {
    let state = state(Vec::new());

    for path in [
        "/v1/node",
        "/v1/displays",
        "/v1/workspaces",
        "/v1/windows",
        "/v1/windows/active",
        "/v1/sunshine",
    ] {
        let (status, _) = call(&state, peer_source(), get(path)).await;

        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path} must be denied");
    }

    let (status, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::new(RemoteCommand::DetachSession),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_an_unlisted_peer_is_denied_while_a_listed_peer_is_served() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let (allowed, _) = call(&state, peer_source(), get("/v1/node")).await;
    let (denied, _) = call(&state, other_source(), get("/v1/node")).await;

    assert_eq!(allowed, StatusCode::OK);
    assert_eq!(denied, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn test_a_read_only_peer_cannot_take_a_session_or_approve_pairing() {
    let state = state(vec![entry(PEER, &[ControlCapability::ReadMetadata])]);

    let (read, _) = call(&state, peer_source(), get("/v1/node")).await;
    assert_eq!(read, StatusCode::OK);

    let (attach, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::new(RemoteCommand::AttachSession {
                role: SessionRole::Remote,
                window: Some(WindowSelector::Address("0x55aa".to_owned())),
                controller: Some(SessionEndpoint {
                    address: peer_source().ip(),
                    port: 48155,
                }),
            }),
        ),
    )
    .await;
    assert_eq!(attach, StatusCode::FORBIDDEN);

    let (pairing, _) = call(
        &state,
        peer_source(),
        post_json("/v1/sunshine/pair/challenge", &serde_json::json!({})),
    )
    .await;
    assert_eq!(pairing, StatusCode::FORBIDDEN);

    let (focus, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/workspaces/focus",
            &serde_json::json!({"target": {"id": 1}}),
        ),
    )
    .await;
    assert_eq!(focus, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_a_command_without_a_request_identifier_is_rejected() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let (status, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &serde_json::json!({"command": {"action": "detach_session"}}),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_a_replayed_command_is_rejected() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);
    let request = CommandRequest::new(RemoteCommand::AttachSession {
        role: SessionRole::Remote,
        window: Some(WindowSelector::Address("0x55aa".to_owned())),
        controller: Some(SessionEndpoint {
            address: peer_source().ip(),
            port: 48155,
        }),
    });

    let (first, _) = call(&state, peer_source(), post_json("/v1/commands", &request)).await;
    let (second, _) = call(&state, peer_source(), post_json("/v1/commands", &request)).await;

    assert_eq!(first, StatusCode::OK);
    assert_eq!(second, StatusCode::CONFLICT);
}

#[tokio::test]
async fn test_a_callback_endpoint_must_match_the_calling_peer() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let (status, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::new(RemoteCommand::AttachSession {
                role: SessionRole::Remote,
                window: Some(WindowSelector::Address("0x55aa".to_owned())),
                controller: Some(SessionEndpoint {
                    address: IpAddr::V4(Ipv4Addr::LOCALHOST),
                    port: 9000,
                }),
            }),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn test_a_second_controller_cannot_steal_or_end_a_live_session() {
    let state = state(vec![
        entry(PEER, &ControlCapability::ALL),
        entry("nOTHER1", &ControlCapability::ALL),
    ]);
    let grant = attach(&state, peer_source()).await;

    let (stolen, _) = call(
        &state,
        other_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::new(RemoteCommand::AttachSession {
                role: SessionRole::Remote,
                window: Some(WindowSelector::Address("0x55aa".to_owned())),
                controller: Some(SessionEndpoint {
                    address: other_source().ip(),
                    port: 48155,
                }),
            }),
        ),
    )
    .await;
    assert_eq!(stolen, StatusCode::CONFLICT);

    let (ended, _) = call(
        &state,
        other_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::for_session(RemoteCommand::DetachSession, grant.claim()),
        ),
    )
    .await;
    assert_eq!(ended, StatusCode::FORBIDDEN);

    assert_eq!(
        state
            .sessions
            .snapshot()
            .await
            .expect("the session survives")
            .owner,
        PEER
    );
}

#[tokio::test]
async fn test_a_stale_detach_does_not_end_the_current_session() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);
    let stale = attach(&state, peer_source()).await;
    let current = attach(&state, peer_source()).await;

    let (status, body) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::for_session(RemoteCommand::DetachSession, stale.claim()),
        ),
    )
    .await;

    assert_eq!(status, StatusCode::OK);
    assert!(!decode::<CommandResponse>(&body).executed);
    assert_eq!(
        state
            .sessions
            .snapshot()
            .await
            .expect("the session survives")
            .id,
        current.id
    );
}

#[tokio::test]
async fn test_the_owner_can_renew_and_end_its_own_session() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);
    let grant = attach(&state, peer_source()).await;

    let (renewed, body) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::for_session(RemoteCommand::RenewSession, grant.claim()),
        ),
    )
    .await;
    assert_eq!(renewed, StatusCode::OK);
    assert_eq!(
        decode::<CommandResponse>(&body)
            .session
            .expect("renewal returns the same session")
            .id,
        grant.id
    );

    let (detached, body) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/commands",
            &CommandRequest::for_session(RemoteCommand::DetachSession, grant.claim()),
        ),
    )
    .await;
    assert_eq!(detached, StatusCode::OK);
    assert!(decode::<CommandResponse>(&body).executed);
    assert!(state.sessions.snapshot().await.is_none());
}

#[tokio::test]
async fn test_pairing_requires_a_live_single_use_challenge() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let (unchallenged, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/sunshine/pair",
            &SunshinePairRequest {
                pairing_id: None,
                pin: "1234".to_owned(),
                client_name: "desktop-a".to_owned(),
            },
        ),
    )
    .await;
    assert_eq!(unchallenged, StatusCode::BAD_REQUEST);

    let (issued, body) = call(
        &state,
        peer_source(),
        post_json("/v1/sunshine/pair/challenge", &serde_json::json!({})),
    )
    .await;
    assert_eq!(issued, StatusCode::OK);
    let pairing_id = decode::<SunshinePairChallengeResponse>(&body).pairing_id;

    let request = SunshinePairRequest {
        pairing_id: Some(pairing_id),
        pin: "1234".to_owned(),
        client_name: "desktop-a".to_owned(),
    };

    let (first, _) = call(
        &state,
        peer_source(),
        post_json("/v1/sunshine/pair", &request),
    )
    .await;
    let (replayed, _) = call(
        &state,
        peer_source(),
        post_json("/v1/sunshine/pair", &request),
    )
    .await;

    assert_eq!(first, StatusCode::OK);
    assert_eq!(replayed, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_a_challenge_issued_to_one_peer_cannot_be_used_by_another() {
    let state = state(vec![
        entry(PEER, &ControlCapability::ALL),
        entry("nOTHER1", &ControlCapability::ALL),
    ]);

    let (_, body) = call(
        &state,
        peer_source(),
        post_json("/v1/sunshine/pair/challenge", &serde_json::json!({})),
    )
    .await;
    let pairing_id = decode::<SunshinePairChallengeResponse>(&body).pairing_id;

    let (status, _) = call(
        &state,
        other_source(),
        post_json(
            "/v1/sunshine/pair",
            &SunshinePairRequest {
                pairing_id: Some(pairing_id),
                pin: "1234".to_owned(),
                client_name: "desktop-b".to_owned(),
            },
        ),
    )
    .await;

    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn test_a_malformed_pin_is_rejected_before_reaching_sunshine() {
    let state = state(vec![entry(PEER, &ControlCapability::ALL)]);

    let (status, _) = call(
        &state,
        peer_source(),
        post_json(
            "/v1/sunshine/pair",
            &SunshinePairRequest {
                pairing_id: Some("abcd".to_owned()),
                pin: "not-a-pin".to_owned(),
                client_name: "desktop-a".to_owned(),
            },
        ),
    )
    .await;

    assert_eq!(status, StatusCode::BAD_REQUEST);
}
