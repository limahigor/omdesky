#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use omdesky_agent::{AgentState, router};
use omdesky_application::ports::MeshNetwork;
use omdesky_platform::{
    access::FileAccessStore,
    agent_client::HttpAgentClient,
    config::{
        Config, access_path, legacy_sunshine_credentials_path, state_dir, sunshine_config_path,
    },
    hyprland::HyprlandAdapter,
    identity::NodeIdentity,
    input::{HyprlandCommandExecutor, HyprlandSessionKeybinds},
    omarchy::{OmarchyNotificationAdapter, detect_version},
    process::TokioCommandRunner,
    sunshine::{SunshineAdapter, SunshineCredentialStore},
    tailscale::TailscaleAdapter,
};
use omdesky_protocol::NodeInfoResponse;
use std::{
    env,
    net::SocketAddr,
    sync::{Arc, Mutex, RwLock},
};
#[cfg(debug_assertions)]
use tracing_subscriber::EnvFilter;

#[cfg(debug_assertions)]
fn init_debug_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();
}

#[cfg(not(debug_assertions))]
fn init_debug_tracing() {}

#[tokio::main]
async fn main() -> Result<()> {
    init_debug_tracing();

    let config = Config::load().context("load configuration")?;
    let runner = Arc::new(TokioCommandRunner);

    let omarchy = detect_version(runner.as_ref())
        .await
        .context("validate Omarchy 4")?;

    let identity = NodeIdentity::load_or_create(&state_dir()?.join("identity/node.json"))
        .context("load stable node identity")?;

    let mesh = Arc::new(TailscaleAdapter::new(runner.clone()));
    let local = mesh
        .local_node()
        .await
        .context("query Tailscale identity")?;
    let address = local
        .addresses
        .iter()
        .find(|address| address.is_ipv4())
        .or(local.addresses.first())
        .copied()
        .context("Tailscale has no local address")?;

    if !is_tailscale_address(address) && !config.network.allow_unsafe_wildcard_bind {
        anyhow::bail!("refusing to bind agent outside the Tailscale address range");
    }

    let desktop = Arc::new(HyprlandAdapter::new(runner.clone()));

    let commands = Arc::new(HyprlandCommandExecutor::new(runner.clone()));

    let keybinds = Arc::new(HyprlandSessionKeybinds::new(runner.clone()));

    let agent_client = Arc::new(HttpAgentClient::new());

    let sunshine = Arc::new(SunshineAdapter::with_api(
        runner,
        sunshine_config_path().context("resolve Sunshine configuration")?,
        omdesky_platform::sunshine::DEFAULT_API_BASE.to_owned(),
        SunshineCredentialStore::new(Some(legacy_sunshine_credentials_path()?)),
    ));

    let notifications = Arc::new(OmarchyNotificationAdapter::default());

    let access = Arc::new(FileAccessStore::new(access_path()?));

    let node = NodeInfoResponse {
        node_id: identity.node_id.to_string(),
        hostname: env::var("HOSTNAME").unwrap_or_else(|_| "omarchy".to_owned()),
        omarchy_version: format!("{}.{}.{}", omarchy.major, omarchy.minor, omarchy.patch),
        agent_version: env!("CARGO_PKG_VERSION").to_owned(),
        protocol_versions: vec![1],
        capabilities: vec![
            "omarchy.node".to_owned(),
            "desktop.stream-host".to_owned(),
            "desktop.input".to_owned(),
            "desktop.audio".to_owned(),
            "hyprland.workspaces".to_owned(),
            "hyprland.windows".to_owned(),
        ],
    };

    let state = AgentState {
        node,
        desktop,
        sunshine,
        mesh,
        access,
        commands,
        keybinds,
        agent_client,
        notifications,
        session: Arc::new(RwLock::new(None)),
        follow_focus: Arc::new(Mutex::new(None)),
        agent_port: config.network.agent_port,
    };

    let listener = tokio::net::TcpListener::bind((address, config.network.agent_port))
        .await
        .context("bind agent to Tailscale address")?;

    tracing::info!(%address, port = config.network.agent_port, "agent.started");

    axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await
    .context("serve agent")
}

fn is_tailscale_address(address: std::net::IpAddr) -> bool {
    match address {
        std::net::IpAddr::V4(address) => {
            let octets = address.octets();

            octets[0] == 100 && (64..128).contains(&octets[1])
        }
        std::net::IpAddr::V6(address) => {
            let segments = address.segments();

            segments[0] == 0xfd7a && segments[1] == 0x115c && segments[2] == 0xa1e0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

    #[test]
    fn test_only_the_cgnat_range_counts_as_tailscale() {
        assert!(is_tailscale_address(
            "100.64.0.1".parse::<IpAddr>().expect("address")
        ));
        assert!(is_tailscale_address(
            "100.127.255.254".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "100.0.0.1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "100.63.255.255".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "100.128.0.1".parse::<IpAddr>().expect("address")
        ));
    }

    #[test]
    fn test_only_the_tailscale_ula_prefix_counts_as_tailscale() {
        assert!(is_tailscale_address(
            "fd7a:115c:a1e0::1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "fd7a:0000:0000::1".parse::<IpAddr>().expect("address")
        ));
        assert!(!is_tailscale_address(
            "fd7a:115c:a1e1::1".parse::<IpAddr>().expect("address")
        ));
    }
}
