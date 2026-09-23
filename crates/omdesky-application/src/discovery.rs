use crate::{
    access::missing_callback_capabilities,
    ports::{
        AccessStore, AgentClient, AgentEndpoint, AllowedController, MeshNetwork, PortError,
        PortResult,
    },
    readiness::ensure_compatible_agent,
};
use futures::{StreamExt, stream};
use omdesky_core::{
    BlockerCode, BlockerSide, ConnectionKind, MeshPeer, NodeBlocker, NodeCapabilities, NodeStatus,
};
use omdesky_protocol::{NodeInfoResponse, RELEASE, ReleaseLine};
use serde::Serialize;
use std::{net::IpAddr, sync::Arc, time::Instant};

const MAX_DISCOVERY_PEERS: usize = 1024;
const DISCOVERY_CONCURRENCY: usize = 16;

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveredNode {
    pub tailnet_node_id: String,
    pub name: String,
    pub address: IpAddr,
    pub status: NodeStatus,
    pub blockers: Vec<NodeBlocker>,
    pub connection: ConnectionKind,
    pub latency_ms: Option<u32>,
    pub agent_version: Option<String>,
    pub omarchy_version: Option<String>,
    pub capabilities: NodeCapabilities,
    pub is_local: bool,
}

impl DiscoveredNode {
    fn from_peer(peer: MeshPeer, name: String, address: IpAddr, is_local: bool) -> Self {
        Self {
            tailnet_node_id: peer.tailnet_node_id,
            name,
            address,
            status: NodeStatus::Unavailable,
            blockers: Vec::new(),
            connection: peer.connection,
            latency_ms: peer.latency_ms,
            agent_version: None,
            omarchy_version: None,
            capabilities: NodeCapabilities::default(),
            is_local,
        }
    }

    fn with_status(mut self, status: NodeStatus) -> Self {
        self.status = status;

        self
    }

    fn with_blockers(mut self, blockers: Vec<NodeBlocker>) -> Self {
        self.status = if blockers.is_empty() {
            NodeStatus::Ready
        } else {
            NodeStatus::Blocked
        };
        self.blockers = blockers;

        self
    }

    fn with_info(mut self, info: Option<NodeInfoResponse>) -> Self {
        if let Some(info) = info {
            self.agent_version = Some(info.agent_version);
            self.omarchy_version = Some(info.omarchy_version);
            self.capabilities = NodeCapabilities::new(info.capabilities);
        }

        self
    }

    pub fn is_connectable(&self) -> bool {
        !self.is_local && self.status == NodeStatus::Ready
    }
}

struct DiscoveryContext {
    allowlist: Vec<AllowedController>,
    local_name: String,
    include_all: bool,
}

pub struct DiscoverNodes {
    mesh: Arc<dyn MeshNetwork>,
    agent: Arc<dyn AgentClient>,
    access: Arc<dyn AccessStore>,
    agent_port: u16,
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

        let context = DiscoveryContext {
            allowlist,
            local_name: local
                .hostname
                .clone()
                .unwrap_or_else(|| local.tailnet_node_id.clone()),
            include_all: include_all_tailnet,
        };

        let local_peer = MeshPeer {
            tailnet_node_id: local.tailnet_node_id,
            dns_name: None,
            hostname: local.hostname,
            ips: local.addresses,
            online: true,
            connection: ConnectionKind::Direct,
            latency_ms: Some(0),
        };

        let mut nodes = Vec::new();

        if let Some(node) = self.probe(local_peer, true, &context).await {
            nodes.push(node);
        }

        let probes = stream::iter(
            peers
                .into_iter()
                .map(|peer| self.probe(peer, false, &context)),
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
        is_local: bool,
        context: &DiscoveryContext,
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

        let online = peer.online;
        let node = DiscoveredNode::from_peer(peer, name, address, is_local);

        if !online {
            return context
                .include_all
                .then(|| node.with_status(NodeStatus::Offline));
        }

        let endpoint = AgentEndpoint {
            address,
            port: self.agent_port,
        };

        let probe_started = Instant::now();

        let Ok(health) = self.agent.health(&endpoint).await else {
            return context.include_all.then_some(node);
        };

        let latency_ms = measured_latency_ms(probe_started.elapsed(), is_local);

        let node = DiscoveredNode {
            latency_ms: Some(latency_ms),
            agent_version: Some(health.agent_version.clone()),
            ..node
        };

        if ensure_compatible_agent(&health).is_err() {
            let blocker = incompatible_blocker(&health.agent_version, &node.name, is_local);

            return Some(node.with_blockers(vec![blocker]));
        }

        let info = self.agent.node_info(&endpoint).await;

        let blockers = match &info {
            Err(PortError {
                code: "VERSION_INCOMPATIBLE",
                ..
            }) => vec![incompatible_blocker(
                &health.agent_version,
                &node.name,
                is_local,
            )],
            _ if is_local => Vec::new(),
            _ => access_blockers(
                info.as_ref().err(),
                context,
                &node.tailnet_node_id,
                &node.name,
            ),
        };

        Some(node.with_blockers(blockers).with_info(info.ok()))
    }
}

pub fn blocker_message(node_name: &str, blocker: &NodeBlocker) -> String {
    let fix = &blocker.fix;

    match blocker.code {
        BlockerCode::Incompatible => {
            format!("{node_name} runs a different Omdesky release. {fix}.")
        }
        BlockerCode::Denied => {
            format!("{node_name} does not allow this computer. Run `{fix}` on {node_name}.")
        }
        BlockerCode::NeedsAccess => {
            format!("This computer does not accept shortcuts from {node_name}. Run `{fix}` here.")
        }
    }
}

fn incompatible_blocker(remote_version: &str, remote_name: &str, is_local: bool) -> NodeBlocker {
    if is_local {
        return NodeBlocker {
            code: BlockerCode::Incompatible,
            side: BlockerSide::Local,
            fix: format!("Restart omdesky-agent so it runs Omdesky {RELEASE}"),
        };
    }

    let remote_is_newer = match (ReleaseLine::parse(remote_version), ReleaseLine::current()) {
        (Some(remote), Some(local)) => remote > local,
        _ => false,
    };

    if remote_is_newer {
        return NodeBlocker {
            code: BlockerCode::Incompatible,
            side: BlockerSide::Local,
            fix: format!("Install Omdesky {remote_version} on this computer"),
        };
    }

    NodeBlocker {
        code: BlockerCode::Incompatible,
        side: BlockerSide::Remote,
        fix: format!("Install Omdesky {RELEASE} on {remote_name}"),
    }
}

fn access_blockers(
    info_error: Option<&PortError>,
    context: &DiscoveryContext,
    tailnet_node_id: &str,
    remote_name: &str,
) -> Vec<NodeBlocker> {
    let mut blockers = Vec::new();

    if let Some(PortError {
        code: "UNAUTHORIZED" | "CAPABILITY_DENIED",
        ..
    }) = info_error
    {
        blockers.push(NodeBlocker {
            code: BlockerCode::Denied,
            side: BlockerSide::Remote,
            fix: format!("omdesky access allow {}", context.local_name),
        });
    }

    if !missing_callback_capabilities(&context.allowlist, tailnet_node_id).is_empty() {
        blockers.push(NodeBlocker {
            code: BlockerCode::NeedsAccess,
            side: BlockerSide::Local,
            fix: format!("omdesky access allow {remote_name}"),
        });
    }

    blockers
}

fn measured_latency_ms(elapsed: std::time::Duration, is_local: bool) -> u32 {
    if is_local {
        return 0;
    }

    u32::try_from(elapsed.as_millis())
        .unwrap_or(u32::MAX)
        .max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{access::allowed, readiness::other_minor_release};
    use omdesky_core::ControlCapability;
    use std::time::Duration;

    fn context(allowlist: Vec<AllowedController>) -> DiscoveryContext {
        DiscoveryContext {
            allowlist,
            local_name: "desk-a".to_owned(),
            include_all: false,
        }
    }

    fn denied(code: &'static str) -> PortError {
        PortError::new(code, "refused", false)
    }

    #[test]
    fn test_a_device_allowed_in_both_directions_has_no_blockers() {
        let context = context(vec![allowed("nREMOTE", &ControlCapability::CALLBACK)]);

        assert!(access_blockers(None, &context, "nREMOTE", "desk-b").is_empty());
    }

    #[test]
    fn test_a_device_that_refuses_this_computer_is_denied_on_the_remote_side() {
        let context = context(vec![allowed("nREMOTE", &ControlCapability::ALL)]);

        for code in ["UNAUTHORIZED", "CAPABILITY_DENIED"] {
            let blockers = access_blockers(Some(&denied(code)), &context, "nREMOTE", "desk-b");

            assert_eq!(
                blockers,
                vec![NodeBlocker {
                    code: BlockerCode::Denied,
                    side: BlockerSide::Remote,
                    fix: "omdesky access allow desk-a".to_owned(),
                }],
                "{code}"
            );
        }
    }

    #[test]
    fn test_a_device_this_computer_does_not_list_needs_local_access() {
        let blockers = access_blockers(None, &context(Vec::new()), "nREMOTE", "desk-b");

        assert_eq!(
            blockers,
            vec![NodeBlocker {
                code: BlockerCode::NeedsAccess,
                side: BlockerSide::Local,
                fix: "omdesky access allow desk-b".to_owned(),
            }]
        );
    }

    #[test]
    fn test_both_missing_entries_are_reported_together() {
        let blockers = access_blockers(
            Some(&denied("UNAUTHORIZED")),
            &context(Vec::new()),
            "nREMOTE",
            "desk-b",
        );

        let codes = blockers
            .iter()
            .map(|blocker| blocker.code)
            .collect::<Vec<_>>();

        assert_eq!(codes, vec![BlockerCode::Denied, BlockerCode::NeedsAccess]);
    }

    #[test]
    fn test_an_older_remote_release_is_fixed_on_the_remote_side() {
        let blocker = incompatible_blocker("0.0.1", "desk-b", false);

        assert_eq!(blocker.side, BlockerSide::Remote);
        assert_eq!(blocker.fix, format!("Install Omdesky {RELEASE} on desk-b"));
    }

    #[test]
    fn test_a_newer_remote_release_is_fixed_on_this_computer() {
        let newer = other_minor_release();

        let blocker = incompatible_blocker(&newer, "desk-b", false);

        assert_eq!(blocker.side, BlockerSide::Local);
        assert_eq!(
            blocker.fix,
            format!("Install Omdesky {newer} on this computer")
        );
    }

    #[test]
    fn test_an_unreadable_remote_release_is_fixed_on_the_remote_side() {
        assert_eq!(
            incompatible_blocker("unknown", "desk-b", false).side,
            BlockerSide::Remote
        );
    }

    #[test]
    fn test_an_outdated_local_agent_is_fixed_by_restarting_it() {
        let blocker = incompatible_blocker("0.0.1", "desk-a", true);

        assert_eq!(blocker.side, BlockerSide::Local);
        assert!(blocker.fix.starts_with("Restart omdesky-agent"));
    }

    #[test]
    fn test_blockers_decide_readiness() {
        let peer = MeshPeer {
            tailnet_node_id: "nREMOTE".to_owned(),
            dns_name: None,
            hostname: Some("desk-b".to_owned()),
            ips: Vec::new(),
            online: true,
            connection: ConnectionKind::Direct,
            latency_ms: None,
        };
        let address = IpAddr::from([100, 64, 0, 2]);
        let node = DiscoveredNode::from_peer(peer, "desk-b".to_owned(), address, false);

        let ready = node.clone().with_blockers(Vec::new());
        let blocked = node.with_blockers(access_blockers(
            None,
            &context(Vec::new()),
            "nREMOTE",
            "desk-b",
        ));

        assert!(ready.is_connectable());
        assert_eq!(blocked.status, NodeStatus::Blocked);
        assert!(!blocked.is_connectable());
    }

    #[test]
    fn test_blocker_message_says_where_to_run_the_fix() {
        let denied = NodeBlocker {
            code: BlockerCode::Denied,
            side: BlockerSide::Remote,
            fix: "omdesky access allow desk-a".to_owned(),
        };
        let needs_access = NodeBlocker {
            code: BlockerCode::NeedsAccess,
            side: BlockerSide::Local,
            fix: "omdesky access allow desk-b".to_owned(),
        };

        assert_eq!(
            blocker_message("desk-b", &denied),
            "desk-b does not allow this computer. Run `omdesky access allow desk-a` on desk-b."
        );
        assert_eq!(
            blocker_message("desk-b", &needs_access),
            "This computer does not accept shortcuts from desk-b. Run `omdesky access allow desk-b` here."
        );
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
        assert_eq!(measured_latency_ms(Duration::from_millis(42), false), 42);
    }
}
