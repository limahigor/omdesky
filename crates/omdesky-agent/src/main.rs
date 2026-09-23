#![forbid(unsafe_code)]

use anyhow::{Context, Result, anyhow};
use omdesky_agent::{
    AgentState, FollowFocusSupervisor,
    authorize::Authorizer,
    pairing::PairingChallenges,
    replay::ReplayGuard,
    router,
    session::{DEFAULT_LEASE, KeybindSessionEffects, SessionCoordinator, run_lease_expiry},
};
use omdesky_application::ports::{MeshNetwork, MeshNodeIdentity, PortError};
use omdesky_core::is_tailscale_address;
use omdesky_platform::{
    access::FileAccessStore,
    agent_client::HttpAgentClient,
    config::{Config, access_path, state_dir, sunshine_config_path},
    hyprland::HyprlandAdapter,
    identity::NodeIdentity,
    input::{HyprlandCommandExecutor, HyprlandSessionKeybinds},
    omarchy::{OmarchyNotificationAdapter, detect_version},
    process::TokioCommandRunner,
    sunshine::{SunshineAdapter, SunshineCredentialStore},
    tailscale::TailscaleAdapter,
};
use omdesky_protocol::{NodeInfoResponse, RELEASE};
use std::{io::IsTerminal, net::SocketAddr, sync::Arc, time::Duration};
use tracing_subscriber::EnvFilter;

const DEFAULT_LOG_FILTER: &str = "info";
const TAILSCALE_STARTUP_ATTEMPTS: u32 = 30;
const TAILSCALE_STARTUP_INTERVAL: Duration = Duration::from_secs(2);

fn init_tracing() {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_LOG_FILTER));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(std::io::stderr().is_terminal())
        .init();
}

enum Invocation {
    Serve,
    Version,
    Help,
}

fn parse_arguments(arguments: impl IntoIterator<Item = String>) -> Result<Invocation> {
    let arguments = arguments.into_iter().collect::<Vec<_>>();

    match arguments.as_slice() {
        [] => Ok(Invocation::Serve),
        [flag] if flag == "--version" || flag == "-V" => Ok(Invocation::Version),
        [flag] if flag == "--help" || flag == "-h" => Ok(Invocation::Help),
        _ => Err(anyhow!(
            "unexpected arguments; run `omdesky-agent --help` for usage"
        )),
    }
}

fn port_failure(error: PortError) -> anyhow::Error {
    anyhow!(
        "{} [{}: {}]",
        error.user_message(),
        error.code,
        error.message
    )
}

async fn wait_for_tailscale(mesh: &TailscaleAdapter) -> Result<MeshNodeIdentity> {
    let mut attempt = 1;

    loop {
        let failure = match mesh.local_node().await {
            Ok(local) if !local.addresses.is_empty() => return Ok(local),
            Ok(_) => "Tailscale has no local address yet".to_owned(),
            Err(error) => format!("{}: {}", error.code, error.message),
        };

        if attempt >= TAILSCALE_STARTUP_ATTEMPTS {
            return Err(anyhow!("Tailscale is not ready: {failure}"));
        }

        tracing::warn!(attempt, detail = %failure, "agent.waiting_for_tailscale");

        tokio::time::sleep(TAILSCALE_STARTUP_INTERVAL).await;

        attempt += 1;
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    match parse_arguments(std::env::args().skip(1))? {
        Invocation::Version => {
            println!("omdesky-agent {RELEASE}");
            return Ok(());
        }
        Invocation::Help => {
            println!(
                "omdesky-agent {RELEASE}\n\nRuns the Omdesky control agent on this computer's Tailscale address.\n\nUsage: omdesky-agent [--version | --help]\n\nLogging is controlled by RUST_LOG (default: {DEFAULT_LOG_FILTER})."
            );
            return Ok(());
        }
        Invocation::Serve => {}
    }

    init_tracing();

    tracing::info!(release = RELEASE, "agent.starting");

    let config = Config::load().context("load configuration")?;
    let runner = Arc::new(TokioCommandRunner);

    let omarchy = detect_version(runner.as_ref())
        .await
        .map_err(port_failure)
        .context("validate Omarchy 4")?;

    let identity = NodeIdentity::load_or_create(&state_dir()?.join("identity/node.json"))
        .context("load stable node identity")?;

    let mesh = Arc::new(TailscaleAdapter::new(runner.clone()));
    let local = wait_for_tailscale(&mesh)
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
        SunshineCredentialStore::default(),
    ));

    let notifications = Arc::new(OmarchyNotificationAdapter::default());

    let access = Arc::new(FileAccessStore::new(access_path()?));

    let node = NodeInfoResponse {
        node_id: identity.node_id.to_string(),
        hostname: local
            .hostname
            .clone()
            .unwrap_or_else(|| local.tailnet_node_id.clone()),
        omarchy_version: format!("{}.{}.{}", omarchy.major, omarchy.minor, omarchy.patch),
        agent_version: RELEASE.to_owned(),
        capabilities: vec![
            "omarchy.node".to_owned(),
            "desktop.stream-host".to_owned(),
            "desktop.input".to_owned(),
            "desktop.audio".to_owned(),
            "hyprland.workspaces".to_owned(),
            "hyprland.windows".to_owned(),
        ],
    };

    let follow_focus =
        FollowFocusSupervisor::new(agent_client.clone(), desktop.clone(), notifications.clone());
    let effects = Arc::new(KeybindSessionEffects::new(keybinds, move |controller| {
        follow_focus.set_controller(controller)
    }));
    let sessions = Arc::new(SessionCoordinator::new(effects, DEFAULT_LEASE));

    if let Err(error) = sessions.reconcile().await {
        tracing::warn!(
            code = error.code,
            detail = %error.message,
            "agent.startup_reconciliation_failed"
        );
    }

    let state = AgentState {
        node,
        desktop,
        sunshine,
        commands,
        agent_client,
        notifications,
        authorizer: Arc::new(Authorizer::with_strict_tailnet_only(
            mesh,
            access,
            config.network.strict_tailnet_only,
        )),
        sessions: sessions.clone(),
        challenges: Arc::new(PairingChallenges::default()),
        replay: Arc::new(ReplayGuard::default()),
        agent_port: config.network.agent_port,
    };

    let listener = tokio::net::TcpListener::bind((address, config.network.agent_port))
        .await
        .context("bind agent to Tailscale address")?;

    tracing::info!(%address, port = config.network.agent_port, "agent.started");

    let expiry = tokio::spawn(run_lease_expiry(sessions.clone()));

    let served = axum::serve(
        listener,
        router(state).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await;

    expiry.abort();
    sessions.release().await;

    served.context("serve agent")
}

async fn shutdown_signal() {
    let interrupt = async {
        let _ = tokio::signal::ctrl_c().await;
    };

    #[cfg(unix)]
    let terminate = async {
        let Ok(mut signal) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        else {
            std::future::pending::<()>().await;
            return;
        };

        signal.recv().await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = interrupt => {}
        () = terminate => {}
    }

    tracing::info!("agent.shutdown_requested");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn invocation(arguments: &[&str]) -> Result<Invocation> {
        parse_arguments(arguments.iter().map(|argument| (*argument).to_owned()))
    }

    #[test]
    fn test_no_arguments_serve_the_agent() {
        assert!(matches!(invocation(&[]), Ok(Invocation::Serve)));
    }

    #[test]
    fn test_version_and_help_flags_are_recognized() {
        assert!(matches!(
            invocation(&["--version"]),
            Ok(Invocation::Version)
        ));
        assert!(matches!(invocation(&["-V"]), Ok(Invocation::Version)));
        assert!(matches!(invocation(&["--help"]), Ok(Invocation::Help)));
        assert!(matches!(invocation(&["-h"]), Ok(Invocation::Help)));
    }

    #[test]
    fn test_unknown_arguments_are_rejected() {
        assert!(invocation(&["--bind", "0.0.0.0"]).is_err());
        assert!(invocation(&["--version", "--help"]).is_err());
    }
}
