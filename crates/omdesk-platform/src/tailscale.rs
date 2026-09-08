use async_trait::async_trait;
use omdesk_application::ports::{
    CommandRunner, CommandSpec, ConnectionInfo, MeshNetwork, MeshNodeIdentity, PortError,
    PortResult,
};
use omdesk_core::{ConnectionKind, MeshPeer};
use serde::Deserialize;
use std::{collections::HashMap, net::IpAddr, sync::Arc};

#[derive(Clone)]
pub struct TailscaleAdapter {
    runner: Arc<dyn CommandRunner>,
}

impl TailscaleAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    async fn status(&self) -> PortResult<TailscaleStatus> {
        let output = self
            .runner
            .run(CommandSpec::new(
                "tailscale",
                ["status".to_owned(), "--json".to_owned()],
            ))
            .await?;
        parse_status(&output.stdout)
    }
}

#[async_trait]
impl MeshNetwork for TailscaleAdapter {
    async fn local_node(&self) -> PortResult<MeshNodeIdentity> {
        let status = self.status().await?;
        Ok(MeshNodeIdentity {
            tailnet_node_id: status.self_node.id,
            user: None,
            hostname: status
                .self_node
                .host_name
                .or_else(|| status.self_node.dns_name.as_deref().map(dns_hostname)),
            addresses: status
                .self_node
                .tailscale_ips
                .into_iter()
                .filter_map(|value| value.parse().ok())
                .collect(),
        })
    }

    async fn peers(&self) -> PortResult<Vec<MeshPeer>> {
        Ok(self
            .status()
            .await?
            .peers
            .into_values()
            .map(raw_peer_to_domain)
            .collect())
    }

    async fn connection_info(&self, tailnet_node_id: &str) -> PortResult<ConnectionInfo> {
        let peer = self
            .status()
            .await?
            .peers
            .into_values()
            .find(|peer| peer.id == tailnet_node_id)
            .ok_or_else(|| PortError::new("PEER_OFFLINE", "peer is not visible", true))?;
        Ok(ConnectionInfo {
            kind: connection_kind(peer.cur_addr.as_deref(), peer.relay.as_deref()),
            latency_ms: None,
        })
    }

    async fn identify_source(&self, source: IpAddr) -> PortResult<Option<MeshNodeIdentity>> {
        let output = self
            .runner
            .run(CommandSpec::new(
                "tailscale",
                ["whois".to_owned(), "--json".to_owned(), source.to_string()],
            ))
            .await;

        match output {
            Ok(output) => Ok(Some(parse_whois(&output.stdout)?)),
            Err(error) if error.code == "COMMAND_FAILED" => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub fn parse_status(bytes: &[u8]) -> PortResult<TailscaleStatus> {
    serde_json::from_slice(bytes).map_err(|error| {
        PortError::new(
            "TAILSCALE_STATUS_INVALID",
            format!("unable to parse tailscale status: {error}"),
            false,
        )
    })
}

pub fn parse_whois(bytes: &[u8]) -> PortResult<MeshNodeIdentity> {
    let whois: WhoisResponse = serde_json::from_slice(bytes).map_err(|error| {
        PortError::new(
            "TAILSCALE_WHOIS_INVALID",
            format!("unable to parse tailscale whois: {error}"),
            false,
        )
    })?;
    Ok(MeshNodeIdentity {
        tailnet_node_id: whois.node.stable_id.unwrap_or_default(),
        user: whois.user_profile.and_then(|profile| profile.login_name),
        hostname: whois
            .node
            .host_name
            .or_else(|| whois.node.name.as_deref().map(dns_hostname)),
        addresses: Vec::new(),
    })
}

fn dns_hostname(dns_name: &str) -> String {
    dns_name
        .trim_end_matches('.')
        .split('.')
        .next()
        .unwrap_or(dns_name)
        .to_owned()
}

fn raw_peer_to_domain(peer: RawPeer) -> MeshPeer {
    MeshPeer {
        tailnet_node_id: peer.id,
        dns_name: peer.dns_name,
        hostname: peer.host_name,
        ips: peer
            .tailscale_ips
            .into_iter()
            .filter_map(|value| value.parse().ok())
            .collect(),
        online: peer.online,
        connection: connection_kind(peer.cur_addr.as_deref(), peer.relay.as_deref()),
        latency_ms: None,
    }
}

fn connection_kind(current_address: Option<&str>, relay: Option<&str>) -> ConnectionKind {
    if current_address.is_some_and(|value| !value.is_empty()) {
        ConnectionKind::Direct
    } else if relay.is_some_and(|value| !value.is_empty()) {
        ConnectionKind::Relay
    } else {
        ConnectionKind::Unknown
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct TailscaleStatus {
    #[serde(rename = "Self")]
    self_node: RawPeer,
    #[serde(default, rename = "Peer")]
    peers: HashMap<String, RawPeer>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct RawPeer {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "DNSName")]
    dns_name: Option<String>,
    host_name: Option<String>,
    #[serde(default, rename = "TailscaleIPs")]
    tailscale_ips: Vec<String>,
    #[serde(default)]
    online: bool,
    cur_addr: Option<String>,
    relay: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisResponse {
    #[serde(rename = "Node")]
    node: WhoisNode,
    #[serde(default, rename = "UserProfile")]
    user_profile: Option<WhoisUser>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisNode {
    #[serde(rename = "StableID")]
    stable_id: Option<String>,
    name: Option<String>,
    host_name: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct WhoisUser {
    login_name: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_status_ignores_unknown_fields() {
        let status = parse_status(include_bytes!(
            "../../../tests/fixtures/tailscale-status-direct.json"
        ))
        .expect("fixture parses");

        assert_eq!(status.self_node.id, "self-node");
        assert_eq!(status.peers.len(), 1);
    }

    #[test]
    fn test_local_node_preserves_tailscale_hostname() {
        let status = parse_status(include_bytes!(
            "../../../tests/fixtures/tailscale-status-direct.json"
        ))
        .expect("fixture parses");

        assert_eq!(status.self_node.host_name.as_deref(), Some("local"));
    }

    #[test]
    fn test_direct_peer_maps_to_direct_connection() {
        let status = parse_status(include_bytes!(
            "../../../tests/fixtures/tailscale-status-direct.json"
        ))
        .expect("fixture parses");
        let peer = raw_peer_to_domain(status.peers.into_values().next().expect("peer"));

        assert_eq!(peer.connection, ConnectionKind::Direct);
        assert!(peer.online);
    }

    #[test]
    fn test_parse_whois_extracts_stable_identity() {
        let identity = parse_whois(
            br#"{"Node":{"StableID":"nABC123","Name":"desktop-a.tail.ts.net.","HostName":"desktop-a"},"UserProfile":{"LoginName":"user@example.com"}}"#,
        )
        .expect("whois parses");

        assert_eq!(identity.tailnet_node_id, "nABC123");
        assert_eq!(identity.user.as_deref(), Some("user@example.com"));
    }
}
