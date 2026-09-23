use crate::ports::{
    AccessStore, AgentClient, AgentEndpoint, AllowedController, ChildProcess, MeshNetwork,
    Notification, NotificationService, PairingState, PortError, PortResult, SessionKeybindConfig,
    SessionKeybindInstaller, StreamClient, StreamHostDescriptor, StreamLaunchRequest,
    StreamWindowLocator,
};
use futures::{StreamExt, stream};
use omdesky_core::{
    ConnectionKind, ControlCapability, InputMode, MeshPeer, NodeCapabilities, NodeStatus,
    RemoteCommand, SessionEndpoint, SessionGrant, SessionRole, StreamProfile, WindowSelector,
    WorkspaceTarget,
};
use omdesky_protocol::{CommandRequest, PROTOCOL, SunshinePairRequest};
use serde::Serialize;
use std::{future::Future, net::IpAddr, sync::Arc, time::Instant};

pub const MOONLIGHT_WINDOW_CLASS: &str = "com.moonlight_stream.Moonlight";

const MAX_DISCOVERY_PEERS: usize = 1024;
const DISCOVERY_CONCURRENCY: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveredNode {
    pub tailnet_node_id: String,
    pub name: String,
    pub address: IpAddr,
    pub status: NodeStatus,
    pub connection: ConnectionKind,
    pub latency_ms: Option<u32>,
    pub agent_version: Option<String>,
    pub omarchy_version: Option<String>,
    pub capabilities: NodeCapabilities,
    pub is_local: bool,
}

pub struct DiscoverNodes {
    mesh: Arc<dyn MeshNetwork>,
    agent: Arc<dyn AgentClient>,
    access: Arc<dyn AccessStore>,
    agent_port: u16,
}

pub fn missing_callback_capabilities(
    allowlist: &[AllowedController],
    tailnet_node_id: &str,
) -> Vec<ControlCapability> {
    let entry = allowlist
        .iter()
        .find(|entry| entry.tailnet_node_id == tailnet_node_id);

    ControlCapability::CALLBACK
        .into_iter()
        .filter(|capability| !entry.is_some_and(|entry| entry.allows(*capability)))
        .collect()
}

pub fn callback_access_error(missing: &[ControlCapability]) -> PortError {
    let missing = missing
        .iter()
        .map(|capability| capability.as_str())
        .collect::<Vec<_>>()
        .join(",");

    PortError::new(
        "CALLBACK_ACCESS_MISSING",
        format!("the controller does not grant {missing} to the remote device"),
        false,
    )
}

#[derive(Clone)]
pub struct CallbackAccess {
    mesh: Arc<dyn MeshNetwork>,
    access: Arc<dyn AccessStore>,
}

impl CallbackAccess {
    pub fn new(mesh: Arc<dyn MeshNetwork>, access: Arc<dyn AccessStore>) -> Self {
        Self { mesh, access }
    }

    pub async fn verify(&self, address: IpAddr) -> PortResult<()> {
        let identity = self.mesh.identify_source(address).await?.ok_or_else(|| {
            PortError::new(
                "PEER_IDENTITY_UNKNOWN",
                "Tailscale does not know the remote address",
                false,
            )
        })?;

        let allowlist = self.access.list().await?;

        let missing = missing_callback_capabilities(&allowlist, &identity.tailnet_node_id);

        if missing.is_empty() {
            return Ok(());
        }

        Err(callback_access_error(&missing))
    }
}

impl DiscoverNodes {
    pub fn new(
        mesh: Arc<dyn MeshNetwork>,
        agent: Arc<dyn AgentClient>,
        access: Arc<dyn AccessStore>,
        agent_port: u16,
    ) -> Self {
        Self {
            mesh,
            agent,
            access,
            agent_port,
        }
    }

    pub async fn execute(&self, include_all_tailnet: bool) -> PortResult<Vec<DiscoveredNode>> {
        let local = self.mesh.local_node().await?;
        let peers = self.mesh.peers().await?;

        if peers.len() > MAX_DISCOVERY_PEERS {
            return Err(PortError::new(
                "DISCOVERY_PEER_LIMIT",
                "the tailnet contains too many peers to discover safely",
                false,
            ));
        }

        let allowlist = self.access.list().await.unwrap_or_else(|error| {
            tracing::debug!(code = error.code, "discovery.allowlist_unavailable");

            Vec::new()
        });

        let mut nodes = Vec::new();

        let local_peer = MeshPeer {
            tailnet_node_id: local.tailnet_node_id,
            dns_name: None,
            hostname: local.hostname,
            ips: local.addresses,
            online: true,
            connection: ConnectionKind::Direct,
            latency_ms: Some(0),
        };

        if let Some(node) = self
            .probe(local_peer, include_all_tailnet, true, &allowlist)
            .await
        {
            nodes.push(node);
        }

        let probes = stream::iter(
            peers
                .into_iter()
                .map(|peer| self.probe(peer, include_all_tailnet, false, &allowlist)),
        )
        .buffer_unordered(DISCOVERY_CONCURRENCY);

        nodes.extend(
            probes
                .filter_map(std::future::ready)
                .collect::<Vec<_>>()
                .await,
        );

        nodes.sort_by(|left, right| {
            right
                .is_local
                .cmp(&left.is_local)
                .then(left.name.cmp(&right.name))
        });

        Ok(nodes)
    }

    async fn probe(
        &self,
        peer: MeshPeer,
        include_all: bool,
        is_local: bool,
        allowlist: &[AllowedController],
    ) -> Option<DiscoveredNode> {
        let address = peer
            .ips
            .iter()
            .find(|ip| ip.is_ipv4())
            .or(peer.ips.first())
            .copied()?;

        let name = peer
            .hostname
            .clone()
            .or(peer.dns_name.clone())
            .unwrap_or_else(|| peer.tailnet_node_id.clone());

        if !peer.online {
            return include_all
                .then(|| generic_node(peer, name, address, NodeStatus::Offline, is_local));
        }

        let endpoint = AgentEndpoint {
            address,
            port: self.agent_port,
        };
        let probe_started = Instant::now();
        match self.agent.health(&endpoint).await {
            Ok(health) if health.protocol == PROTOCOL => {
                let latency_ms = measured_latency_ms(probe_started.elapsed(), is_local);
                let info = self.agent.node_info(&endpoint).await;

                let status = peer_status(&info, allowlist, &peer.tailnet_node_id, is_local);

                let info = info.ok();

                Some(DiscoveredNode {
                    tailnet_node_id: peer.tailnet_node_id,
                    name,
                    address,
                    status,
                    connection: peer.connection,
                    latency_ms: Some(latency_ms),
                    agent_version: info.as_ref().map(|value| value.agent_version.clone()),
                    omarchy_version: info.as_ref().map(|value| value.omarchy_version.clone()),
                    capabilities: info
                        .map(|value| NodeCapabilities::new(value.capabilities))
                        .unwrap_or_default(),
                    is_local,
                })
            }
            Ok(_)
            | Err(PortError {
                code: "VERSION_INCOMPATIBLE",
                ..
            }) => Some(generic_node(
                peer,
                name,
                address,
                NodeStatus::Incompatible,
                is_local,
            )),
            Err(_) if include_all => Some(generic_node(
                peer,
                name,
                address,
                NodeStatus::AgentUnknown,
                is_local,
            )),
            _ => None,
        }
    }
}

pub struct PairStream {
    agent: Arc<dyn AgentClient>,
    stream: Arc<dyn StreamClient>,
    client_name: String,
}

impl PairStream {
    pub fn new(
        agent: Arc<dyn AgentClient>,
        stream: Arc<dyn StreamClient>,
        client_name: String,
    ) -> Self {
        Self {
            agent,
            stream,
            client_name,
        }
    }

    pub async fn execute(&self, endpoint: &AgentEndpoint) -> PortResult<()> {
        let host = StreamHostDescriptor {
            address: endpoint.address,
            application: "Desktop".to_owned(),
        };

        if matches!(
            self.stream.pairing_state(&host).await?,
            PairingState::Paired
        ) {
            return Ok(());
        }

        let status = self.agent.sunshine_status(endpoint).await?;

        if !status.running {
            return Err(PortError::new(
                "SUNSHINE_NOT_RUNNING",
                "Sunshine is not running on the remote node",
                true,
            ));
        }

        if !status.pairing_api_available {
            return Err(PortError::new(
                "SUNSHINE_API_UNAVAILABLE",
                "Sunshine admin credentials are missing on the remote host. Configure them on that machine (Settings -> Configure Sunshine credentials, or `omdesky setup`), not on this controller.",
                false,
            ));
        }

        let challenge = self.agent.sunshine_pair_challenge(endpoint).await?;
        let pending = self.stream.begin_pairing(&host).await?;

        self.agent
            .sunshine_pair(
                endpoint,
                SunshinePairRequest {
                    pairing_id: challenge.pairing_id,
                    pin: pending.pin,
                    client_name: self.client_name.clone(),
                },
            )
            .await?;

        match self.stream.pairing_state(&host).await? {
            PairingState::Paired => Ok(()),
            _ => Err(PortError::new(
                "MOONLIGHT_PAIRING_FAILED",
                "Moonlight did not report the host as paired",
                true,
            )),
        }
    }
}

pub struct ConnectRequest {
    pub endpoint: AgentEndpoint,
    pub controller_endpoint: AgentEndpoint,
    pub profile: StreamProfile,
    pub fullscreen: bool,
    pub input_mode: InputMode,
    pub focus_workspace: Option<WorkspaceTarget>,
    pub focus_window: Option<String>,
    pub auto_pair: bool,
}

pub const STREAM_WINDOW_LOOKUP_ATTEMPTS: u32 = 20;
pub const STREAM_WINDOW_LOOKUP_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);

pub struct ConnectNode {
    agent: Arc<dyn AgentClient>,
    callback_access: CallbackAccess,
    stream: Arc<dyn StreamClient>,
    keybinds: Arc<dyn SessionKeybindInstaller>,
    notifications: Arc<dyn NotificationService>,
    windows: Option<Arc<dyn StreamWindowLocator>>,
    pairing: PairStream,
}

impl ConnectNode {
    pub fn new(
        agent: Arc<dyn AgentClient>,
        callback_access: CallbackAccess,
        stream: Arc<dyn StreamClient>,
        keybinds: Arc<dyn SessionKeybindInstaller>,
        notifications: Arc<dyn NotificationService>,
        client_name: String,
    ) -> Self {
        let pairing = PairStream::new(agent.clone(), stream.clone(), client_name);
        Self {
            agent,
            callback_access,
            stream,
            keybinds,
            notifications,
            windows: None,
            pairing,
        }
    }

    pub fn with_window_locator(mut self, windows: Arc<dyn StreamWindowLocator>) -> Self {
        self.windows = Some(windows);

        self
    }

    async fn locate_stream_window(&self, pid: Option<u32>) -> WindowSelector {
        let fallback = WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned());

        let (Some(windows), Some(pid)) = (self.windows.as_ref(), pid) else {
            return fallback;
        };

        for _ in 0..STREAM_WINDOW_LOOKUP_ATTEMPTS {
            match windows.window_for_process(pid).await {
                Ok(Some(window)) => return window,
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(
                        code = error.code,
                        detail = %error.message,
                        "session.window_lookup_failed"
                    );

                    return fallback;
                }
            }

            tokio::time::sleep(STREAM_WINDOW_LOOKUP_INTERVAL).await;
        }

        tracing::warn!("session.window_lookup_timed_out");

        fallback
    }

    pub async fn displays(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<Vec<omdesky_core::Display>> {
        self.agent.displays(endpoint).await
    }

    pub async fn execute(&self, request: ConnectRequest) -> PortResult<i32> {
        self.execute_with_started(request, |_| async {}).await
    }

    pub async fn execute_with_started<F, Fut>(
        &self,
        request: ConnectRequest,
        on_started: F,
    ) -> PortResult<i32>
    where
        F: FnOnce(Option<u32>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        self.callback_access
            .verify(request.endpoint.address)
            .await?;

        let status = self.agent.sunshine_status(&request.endpoint).await?;

        if !status.ready {
            return Err(PortError::new(
                "SUNSHINE_NOT_READY",
                "Sunshine is not ready to stream on the remote node",
                true,
            ));
        }

        let host = StreamHostDescriptor {
            address: request.endpoint.address,
            application: status.desktop_app.unwrap_or_else(|| "Desktop".to_owned()),
        };

        if !matches!(
            self.stream.pairing_state(&host).await?,
            PairingState::Paired
        ) {
            if request.auto_pair {
                self.pairing.execute(&request.endpoint).await?;
            } else {
                return Err(PortError::new(
                    "MOONLIGHT_PAIRING_REQUIRED",
                    "Moonlight is not paired with the host; run `omdesky pair` first",
                    false,
                ));
            }
        }

        if let Some(target) = request.focus_workspace.clone() {
            self.agent
                .focus_workspace(&request.endpoint, target)
                .await?;
        }

        if let Some(window) = &request.focus_window {
            self.agent.focus_window(&request.endpoint, window).await?;
        }

        let stream_request = StreamLaunchRequest {
            host,
            profile: request.profile.clone(),
            fullscreen: request.fullscreen,
            input_mode: request.input_mode,
        };

        verify_agents(
            self.agent.as_ref(),
            &request.controller_endpoint,
            &request.endpoint,
        )
        .await?;
        let process = self.stream.launch(stream_request).await?;
        let moonlight_pid = process.id();

        let mut grant = None;
        let mut heartbeat = None;

        if request.input_mode == InputMode::Remote {
            let window = self.locate_stream_window(moonlight_pid).await;

            match self.attach_session(&request, window).await {
                Ok(session) => {
                    heartbeat = Some(SessionHeartbeat::spawn(
                        self.agent.clone(),
                        request.endpoint.clone(),
                        session,
                    ));
                    grant = Some(session);
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: "Remote shortcuts are ready. Press SUPER+R to switch shortcut capture.".to_owned(),
                        })
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        code = error.code,
                        detail = %error.message,
                        "session.shortcuts_setup_failed"
                    );
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: format!(
                                "The stream started, but remote shortcuts are unavailable. {}",
                                error.user_message()
                            ),
                        })
                        .await;
                }
            }
        }

        on_started(moonlight_pid).await;

        let exit = wait_for_process_or_monitor(
            process,
            monitor_agents(
                self.agent.clone(),
                request.controller_endpoint.clone(),
                request.endpoint.clone(),
            ),
        )
        .await;

        drop(heartbeat);

        if let Some(grant) = grant {
            match self.detach_session(&request, grant).await {
                Ok(()) => {
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: "The stream ended and your local shortcuts were restored."
                                .to_owned(),
                        })
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        code = error.code,
                        detail = %error.message,
                        "session.shortcuts_restore_failed"
                    );
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: "The stream ended, but local shortcuts could not be restored. Restart Hyprland before reconnecting.".to_owned(),
                        })
                        .await;
                }
            }
        }

        exit
    }

    async fn attach_session(
        &self,
        request: &ConnectRequest,
        window: WindowSelector,
    ) -> PortResult<SessionGrant> {
        self.keybinds
            .install(SessionKeybindConfig {
                role: SessionRole::Controller,
                controller: None,
                window: window.clone(),
            })
            .await?;

        let controller = SessionEndpoint {
            address: request.controller_endpoint.address,
            port: request.controller_endpoint.port,
        };

        let attach = self
            .agent
            .send_command(
                &request.endpoint,
                CommandRequest::new(RemoteCommand::AttachSession {
                    role: SessionRole::Remote,
                    controller: Some(controller),
                    window: Some(window),
                }),
            )
            .await;

        match attach {
            Ok(response) => response.session.ok_or_else(|| {
                PortError::new(
                    "SESSION_NOT_GRANTED",
                    "the remote agent accepted the session without issuing a lease",
                    false,
                )
            }),
            Err(error) => {
                let _ = self.keybinds.clear().await;
                Err(error)
            }
        }
    }

    async fn detach_session(
        &self,
        request: &ConnectRequest,
        grant: SessionGrant,
    ) -> PortResult<()> {
        let local = self.keybinds.clear().await;
        let remote = self
            .agent
            .send_command(
                &request.endpoint,
                CommandRequest::for_session(RemoteCommand::DetachSession, grant.claim()),
            )
            .await;

        local.and(remote.map(|_| ()))
    }
}

const AGENT_HEALTH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
const AGENT_HEALTH_FAILURE_LIMIT: u8 = 2;

const MIN_RENEWAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct SessionHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl SessionHeartbeat {
    fn spawn(agent: Arc<dyn AgentClient>, endpoint: AgentEndpoint, grant: SessionGrant) -> Self {
        let interval = renewal_interval(grant.lease_seconds);

        Self {
            handle: tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await;

                loop {
                    ticker.tick().await;

                    let renewal = agent
                        .send_command(
                            &endpoint,
                            CommandRequest::for_session(RemoteCommand::RenewSession, grant.claim()),
                        )
                        .await;

                    if let Err(error) = renewal {
                        tracing::debug!(
                            code = error.code,
                            detail = %error.message,
                            "session.renewal_failed"
                        );
                    }
                }
            }),
        }
    }
}

impl Drop for SessionHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn renewal_interval(lease_seconds: u64) -> std::time::Duration {
    std::time::Duration::from_secs(lease_seconds.max(1) / 3).max(MIN_RENEWAL_INTERVAL)
}

fn agent_health_failure(
    local_failures: u8,
    remote_failures: u8,
    failure_limit: u8,
) -> Option<PortError> {
    if local_failures >= failure_limit {
        Some(PortError::new(
            "LOCAL_AGENT_UNAVAILABLE",
            "the local omdesky-agent stopped responding",
            true,
        ))
    } else if remote_failures >= failure_limit {
        Some(PortError::new(
            "REMOTE_AGENT_UNAVAILABLE",
            "the remote omdesky-agent stopped responding",
            true,
        ))
    } else {
        None
    }
}

async fn verify_agents(
    agent: &dyn AgentClient,
    local: &AgentEndpoint,
    remote: &AgentEndpoint,
) -> PortResult<()> {
    let (local_result, remote_result) = tokio::join!(agent.health(local), agent.health(remote));

    match agent_health_failure(
        u8::from(local_result.is_err()),
        u8::from(remote_result.is_err()),
        1,
    ) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn monitor_agents(
    agent: Arc<dyn AgentClient>,
    local: AgentEndpoint,
    remote: AgentEndpoint,
) -> PortError {
    let mut interval = tokio::time::interval(AGENT_HEALTH_INTERVAL);
    let mut local_failures = 0;
    let mut remote_failures = 0;
    interval.tick().await;

    loop {
        interval.tick().await;
        let (local_result, remote_result) =
            tokio::join!(agent.health(&local), agent.health(&remote));
        local_failures = if local_result.is_ok() {
            0
        } else {
            local_failures + 1
        };
        remote_failures = if remote_result.is_ok() {
            0
        } else {
            remote_failures + 1
        };

        if let Some(error) =
            agent_health_failure(local_failures, remote_failures, AGENT_HEALTH_FAILURE_LIMIT)
        {
            return error;
        }
    }
}

pub async fn wait_for_process_or_monitor<F>(
    mut process: Box<dyn ChildProcess>,
    monitor: F,
) -> PortResult<i32>
where
    F: Future<Output = PortError>,
{
    tokio::pin!(monitor);

    tokio::select! {
        exit = process.wait() => exit,
        error = &mut monitor => {
            if let Err(terminate_error) = process.terminate().await {
                tracing::warn!(
                    code = terminate_error.code,
                    detail = %terminate_error.message,
                    "session.stream_terminate_failed"
                );
            }
            Err(error)
        }
    }
}

pub fn ensure_controller_ready(
    local_agent: PortResult<()>,
    sunshine_configured: bool,
) -> PortResult<()> {
    match local_agent {
        Ok(()) => {}
        Err(error) if error.code == "VERSION_INCOMPATIBLE" => {
            return Err(PortError::new(
                "LOCAL_AGENT_INCOMPATIBLE",
                error.message,
                false,
            ));
        }
        Err(_) => {
            return Err(PortError::new(
                "LOCAL_AGENT_UNAVAILABLE",
                "the local omdesky-agent is not running",
                true,
            ));
        }
    }

    if !sunshine_configured {
        return Err(PortError::new(
            "LOCAL_SUNSHINE_UNCONFIGURED",
            "Sunshine credentials are not configured on this controller",
            false,
        ));
    }

    Ok(())
}

fn measured_latency_ms(elapsed: std::time::Duration, is_local: bool) -> u32 {
    if is_local {
        return 0;
    }

    u32::try_from(elapsed.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

fn peer_status<T>(
    info: &PortResult<T>,
    allowlist: &[AllowedController],
    tailnet_node_id: &str,
    is_local: bool,
) -> NodeStatus {
    if is_local {
        return NodeStatus::Ready;
    }

    if let Err(PortError {
        code: "VERSION_INCOMPATIBLE",
        ..
    }) = info
    {
        return NodeStatus::Incompatible;
    }

    if let Err(PortError {
        code: "UNAUTHORIZED" | "CAPABILITY_DENIED",
        ..
    }) = info
    {
        return NodeStatus::Denied;
    }

    if !missing_callback_capabilities(allowlist, tailnet_node_id).is_empty() {
        return NodeStatus::NeedsAccess;
    }

    NodeStatus::Ready
}

fn generic_node(
    peer: MeshPeer,
    name: String,
    address: IpAddr,
    status: NodeStatus,
    is_local: bool,
) -> DiscoveredNode {
    DiscoveredNode {
        tailnet_node_id: peer.tailnet_node_id,
        name,
        address,
        status,
        connection: peer.connection,
        latency_ms: peer.latency_ms,
        agent_version: None,
        omarchy_version: None,
        capabilities: NodeCapabilities::default(),
        is_local,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    struct PendingProcess {
        terminated: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::ports::ChildProcess for PendingProcess {
        fn id(&self) -> Option<u32> {
            Some(42)
        }

        async fn wait(&mut self) -> PortResult<i32> {
            std::future::pending().await
        }

        async fn terminate(&mut self) -> PortResult<()> {
            self.terminated
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_wait_for_process_or_monitor_terminates_stream_on_health_failure() {
        let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let process: Box<dyn crate::ports::ChildProcess> = Box::new(PendingProcess {
            terminated: terminated.clone(),
        });
        let failure = PortError::new("LOCAL_AGENT_UNAVAILABLE", "agent stopped", true);

        let result = wait_for_process_or_monitor(process, async { failure }).await;

        assert_eq!(
            result.expect_err("monitor failure ends session").code,
            "LOCAL_AGENT_UNAVAILABLE"
        );
        assert!(terminated.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_agent_health_failure_identifies_local_agent() {
        let error = agent_health_failure(2, 0, 2).expect("local failure");

        assert_eq!(error.code, "LOCAL_AGENT_UNAVAILABLE");
    }

    #[test]
    fn test_agent_health_failure_identifies_remote_agent() {
        let error = agent_health_failure(0, 2, 2).expect("remote failure");

        assert_eq!(error.code, "REMOTE_AGENT_UNAVAILABLE");
    }

    #[test]
    fn test_agent_health_failure_accepts_healthy_agents() {
        assert!(agent_health_failure(0, 0, 2).is_none());
    }

    #[test]
    fn test_agent_health_failure_tolerates_one_transient_failure() {
        assert!(agent_health_failure(1, 1, 2).is_none());
    }

    fn allowed(id: &str, capabilities: &[ControlCapability]) -> AllowedController {
        AllowedController::new(
            id,
            None,
            time::OffsetDateTime::UNIX_EPOCH,
            capabilities.iter().copied(),
        )
    }

    #[test]
    fn test_missing_callback_capabilities_reports_an_unlisted_device() {
        let allowlist = vec![allowed("nOTHER", &ControlCapability::ALL)];

        assert_eq!(
            missing_callback_capabilities(&allowlist, "nREMOTE"),
            ControlCapability::CALLBACK.to_vec()
        );
    }

    #[test]
    fn test_missing_callback_capabilities_reports_each_withheld_capability() {
        let allowlist = vec![allowed("nREMOTE", &[ControlCapability::SendShortcut])];

        assert_eq!(
            missing_callback_capabilities(&allowlist, "nREMOTE"),
            vec![ControlCapability::CloseStream]
        );
    }

    #[test]
    fn test_missing_callback_capabilities_accepts_a_device_with_the_callbacks() {
        let allowlist = vec![allowed("nREMOTE", &ControlCapability::CALLBACK)];

        assert!(missing_callback_capabilities(&allowlist, "nREMOTE").is_empty());
    }

    #[test]
    fn test_peer_status_reports_a_device_that_refuses_this_computer() {
        let allowlist = vec![allowed("nREMOTE", &ControlCapability::ALL)];

        for code in ["UNAUTHORIZED", "CAPABILITY_DENIED"] {
            let info: PortResult<()> = Err(PortError::new(code, "refused", false));

            assert_eq!(
                peer_status(&info, &allowlist, "nREMOTE", false),
                NodeStatus::Denied,
                "{code}"
            );
        }
    }

    #[test]
    fn test_peer_status_reports_a_device_this_computer_does_not_list() {
        let info: PortResult<()> = Ok(());

        assert_eq!(
            peer_status(&info, &[], "nREMOTE", false),
            NodeStatus::NeedsAccess
        );
    }

    #[test]
    fn test_peer_status_accepts_a_device_allowed_in_both_directions() {
        let allowlist = vec![allowed("nREMOTE", &ControlCapability::CALLBACK)];
        let info: PortResult<()> = Ok(());

        assert_eq!(
            peer_status(&info, &allowlist, "nREMOTE", false),
            NodeStatus::Ready
        );
    }

    #[test]
    fn test_peer_status_reports_a_device_whose_metadata_is_from_another_release() {
        let allowlist = vec![allowed("nREMOTE", &ControlCapability::ALL)];
        let info: PortResult<()> = Err(PortError::new("VERSION_INCOMPATIBLE", "0.1.1", false));

        assert_eq!(
            peer_status(&info, &allowlist, "nREMOTE", false),
            NodeStatus::Incompatible
        );
    }

    #[test]
    fn test_peer_status_keeps_this_computer_ready() {
        let info: PortResult<()> = Err(PortError::new("UNAUTHORIZED", "refused", false));

        assert_eq!(peer_status(&info, &[], "nSELF", true), NodeStatus::Ready);
    }

    #[test]
    fn test_ensure_controller_ready_rejects_inactive_local_agent() {
        let local_agent = Err(PortError::new(
            "AGENT_UNREACHABLE",
            "connection refused",
            true,
        ));

        let error =
            ensure_controller_ready(local_agent, true).expect_err("inactive agent is rejected");

        assert_eq!(error.code, "LOCAL_AGENT_UNAVAILABLE");
    }

    #[test]
    fn test_ensure_controller_ready_rejects_local_agent_from_another_release() {
        let local_agent = Err(PortError::new(
            "VERSION_INCOMPATIBLE",
            "the device runs Omdesky 0.1.1",
            false,
        ));

        let error = ensure_controller_ready(local_agent, true)
            .expect_err("an agent from another release is rejected");

        assert_eq!(error.code, "LOCAL_AGENT_INCOMPATIBLE");
        assert!(!error.retryable);
    }

    #[test]
    fn test_ensure_controller_ready_rejects_missing_sunshine_configuration() {
        let error = ensure_controller_ready(Ok(()), false).expect_err("missing setup is rejected");

        assert_eq!(error.code, "LOCAL_SUNSHINE_UNCONFIGURED");
    }

    #[test]
    fn test_ensure_controller_ready_accepts_configured_controller() {
        assert!(ensure_controller_ready(Ok(()), true).is_ok());
    }

    #[test]
    fn test_measured_latency_reports_zero_for_local_device() {
        assert_eq!(measured_latency_ms(Duration::from_micros(500), true), 0);
    }

    #[test]
    fn test_measured_latency_reports_at_least_one_millisecond_for_remote_device() {
        assert_eq!(measured_latency_ms(Duration::from_micros(500), false), 1);
    }

    #[test]
    fn test_measured_latency_preserves_remote_elapsed_milliseconds() {
        assert_eq!(measured_latency_ms(Duration::from_millis(37), false), 37);
    }
}
