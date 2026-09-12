use crate::ports::{
    AgentClient, AgentEndpoint, MeshNetwork, Notification, NotificationService, PairingState,
    PortError, PortResult, SessionKeybindConfig, SessionKeybindInstaller, StreamClient,
    StreamHostDescriptor, StreamLaunchRequest,
};
use omdesk_core::{
    ConnectionKind, InputMode, MeshPeer, NodeCapabilities, NodeStatus, RemoteCommand,
    SessionEndpoint, SessionRole, SessionState, StreamProfile, WorkspaceTarget,
};
use omdesk_protocol::{PROTOCOL_V1, SunshinePairRequest};
use serde::Serialize;
use std::{net::IpAddr, sync::Arc, time::Instant};

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
    agent_port: u16,
}

impl DiscoverNodes {
    pub fn new(mesh: Arc<dyn MeshNetwork>, agent: Arc<dyn AgentClient>, agent_port: u16) -> Self {
        Self {
            mesh,
            agent,
            agent_port,
        }
    }

    pub async fn execute(&self, include_all_tailnet: bool) -> PortResult<Vec<DiscoveredNode>> {
        let local = self.mesh.local_node().await?;
        let peers = self.mesh.peers().await?;

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

        if let Some(node) = self.probe(local_peer, include_all_tailnet, true).await {
            nodes.push(node);
        }

        let probes = peers
            .into_iter()
            .map(|peer| self.probe(peer, include_all_tailnet, false));

        nodes.extend(
            futures::future::join_all(probes)
                .await
                .into_iter()
                .flatten(),
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
    ) -> Option<DiscoveredNode> {
        let address = peer
            .ips
            .iter()
            .find(|ip| ip.is_ipv4())
            .or(peer.ips.first())
            .copied()?;

        let endpoint = AgentEndpoint {
            address,
            port: self.agent_port,
        };

        let name = peer
            .hostname
            .clone()
            .or(peer.dns_name.clone())
            .unwrap_or_else(|| peer.tailnet_node_id.clone());

        let probe_started = Instant::now();
        match self.agent.health(&endpoint).await {
            Ok(health) if health.protocol == PROTOCOL_V1 => {
                let latency_ms = measured_latency_ms(probe_started.elapsed(), is_local);
                let info = self.agent.node_info(&endpoint).await.ok();
                Some(DiscoveredNode {
                    tailnet_node_id: peer.tailnet_node_id,
                    name,
                    address,
                    status: NodeStatus::Ready,
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
            Ok(_) if include_all => Some(generic_node(
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
                "Sunshine admin credentials are missing on the remote host. Configure them on that machine (Settings -> Configure Sunshine credentials, or `omdesk setup`), not on this controller.",
                false,
            ));
        }

        let pending = self.stream.begin_pairing(&host).await?;
        self.agent
            .sunshine_pair(
                endpoint,
                SunshinePairRequest {
                    pairing_id: None,
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

pub struct ConnectNode {
    agent: Arc<dyn AgentClient>,
    stream: Arc<dyn StreamClient>,
    keybinds: Arc<dyn SessionKeybindInstaller>,
    notifications: Arc<dyn NotificationService>,
    pairing: PairStream,
}

impl ConnectNode {
    pub fn new(
        agent: Arc<dyn AgentClient>,
        stream: Arc<dyn StreamClient>,
        keybinds: Arc<dyn SessionKeybindInstaller>,
        notifications: Arc<dyn NotificationService>,
        client_name: String,
    ) -> Self {
        let pairing = PairStream::new(agent.clone(), stream.clone(), client_name);
        Self {
            agent,
            stream,
            keybinds,
            notifications,
            pairing,
        }
    }

    pub async fn displays(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<Vec<omdesk_core::Display>> {
        self.agent.displays(endpoint).await
    }

    pub async fn execute(&self, request: ConnectRequest) -> PortResult<i32> {
        self.execute_with_started(request, || async {}).await
    }

    pub async fn execute_with_started<F, Fut>(
        &self,
        request: ConnectRequest,
        on_started: F,
    ) -> PortResult<i32>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
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
                    "Moonlight is not paired with the host; run `omdesk pair` first",
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

        let mut process = self.stream.launch(stream_request).await?;

        if request.input_mode == InputMode::Remote {
            match self.attach_session(&request).await {
                Ok(()) => {
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omarchy Desk".to_owned(),
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
                            summary: "Omarchy Desk".to_owned(),
                            body: "The stream started, but remote shortcuts are unavailable."
                                .to_owned(),
                        })
                        .await;
                }
            }
        }

        on_started().await;

        let exit = process.wait().await;

        if request.input_mode == InputMode::Remote {
            match self.detach_session(&request).await {
                Ok(()) => {
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omarchy Desk".to_owned(),
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
                            summary: "Omarchy Desk".to_owned(),
                            body: "The stream ended, but local shortcuts could not be restored. Restart Hyprland before reconnecting.".to_owned(),
                        })
                        .await;
                }
            }
        }

        exit
    }

    async fn attach_session(&self, request: &ConnectRequest) -> PortResult<()> {
        self.keybinds
            .install(SessionKeybindConfig {
                role: SessionRole::Controller,
                controller: None,
            })
            .await?;

        let controller = SessionEndpoint {
            address: request.controller_endpoint.address,
            port: request.controller_endpoint.port,
        };

        if let Err(error) = self
            .agent
            .send_command(
                &request.endpoint,
                RemoteCommand::AttachSession {
                    role: SessionRole::Remote,
                    controller: Some(controller),
                },
            )
            .await
        {
            let _ = self.keybinds.clear().await;
            return Err(error);
        }

        Ok(())
    }

    async fn detach_session(&self, request: &ConnectRequest) -> PortResult<()> {
        let local = self.keybinds.clear().await;
        let remote = self
            .agent
            .send_command(&request.endpoint, RemoteCommand::DetachSession)
            .await;

        local.and(remote)
    }
}

pub fn next_state(current: SessionState, next: SessionState) -> PortResult<SessionState> {
    current
        .transition(next)
        .map_err(|error| PortError::new("INVALID_SESSION_TRANSITION", error.to_string(), false))
}

fn measured_latency_ms(elapsed: std::time::Duration, is_local: bool) -> u32 {
    if is_local {
        return 0;
    }

    u32::try_from(elapsed.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
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
