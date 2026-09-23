use anyhow::{Context, Result};
use omdesky_application::{
    discovery::{DiscoverNodes, DiscoveredNode, blocker_message},
    ports::{AccessStore, AgentClient, AllowedController, MeshNetwork, RemoteOmarchy, StreamHost},
    readiness::ensure_compatible_agent,
};
use omdesky_core::NodeStatus;
use omdesky_platform::{
    access::FileAccessStore,
    config::{Config, access_path, sunshine_config_path},
    hyprland::HyprlandAdapter,
    omarchy::detect_version,
    process::TokioCommandRunner,
    sunshine::{DEFAULT_API_BASE, SunshineAdapter, SunshineCredentialStore},
    systemd::{AGENT_UNIT, agent_service_executable},
    tailscale::TailscaleAdapter,
};
use omdesky_protocol::{HealthResponse, PROTOCOL, RELEASE};
use serde_json::json;
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use crate::{agent_client, local_agent_endpoint, print_json};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Status {
    Pass,
    Warn,
    Fail,
}

impl Status {
    fn as_str(self) -> &'static str {
        match self {
            Status::Pass => "PASS",
            Status::Warn => "WARN",
            Status::Fail => "FAIL",
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct Check {
    status: Status,
    message: String,
}

impl Check {
    fn pass(message: impl Into<String>) -> Self {
        Self {
            status: Status::Pass,
            message: message.into(),
        }
    }

    fn warn(message: impl Into<String>) -> Self {
        Self {
            status: Status::Warn,
            message: message.into(),
        }
    }

    fn fail(message: impl Into<String>) -> Self {
        Self {
            status: Status::Fail,
            message: message.into(),
        }
    }

    fn from_result<T, E: std::fmt::Display>(result: &Result<T, E>) -> Self {
        match result {
            Ok(_) => Self::pass("available"),
            Err(error) => Self::fail(error.to_string()),
        }
    }
}

pub async fn run(json: bool) -> Result<()> {
    let config = Config::load()?;
    let runner = Arc::new(TokioCommandRunner);
    let mesh = Arc::new(TailscaleAdapter::new(runner.clone()));
    let access = Arc::new(FileAccessStore::new(access_path()?));

    let omarchy = detect_version(runner.as_ref()).await;
    let tailscale = mesh.local_node().await;
    let hyprland = HyprlandAdapter::new(runner.clone()).displays().await;
    let sunshine = SunshineAdapter::with_api(
        runner.clone(),
        sunshine_config_path()?,
        DEFAULT_API_BASE.to_owned(),
        SunshineCredentialStore::default(),
    )
    .readiness()
    .await;

    let credential_store = SunshineCredentialStore::default();
    let credentials = tokio::task::spawn_blocking(move || credential_store.load())
        .await
        .context("query desktop Secret Service")?;

    let agent_health = match local_agent_endpoint(config.network.agent_port).await {
        Ok(endpoint) => agent_client().health(&endpoint).await.ok(),
        Err(_) => None,
    };

    let service_executable = agent_service_executable(runner.as_ref()).await;

    let allowlist = access.list().await;

    let devices = DiscoverNodes::new(mesh, agent_client(), access, config.network.agent_port)
        .execute(false)
        .await;

    let checks = vec![
        ("omarchy", Check::from_result(&omarchy)),
        ("tailscale", Check::from_result(&tailscale)),
        ("hyprland", Check::from_result(&hyprland)),
        ("sunshine", Check::from_result(&sunshine)),
        ("sunshine_pairing", sunshine_pairing_check(&credentials)),
        (
            "agent",
            agent_check(agent_health.as_ref(), config.network.agent_port),
        ),
        (
            "agent_service",
            agent_service_check(
                service_executable.ok().flatten().as_deref(),
                std::env::current_exe().ok().as_deref(),
            ),
        ),
        ("access", access_check(allowlist.as_deref().ok())),
        ("devices", devices_check(devices.as_deref())),
    ];

    report(&checks, json)
}

fn report(checks: &[(&'static str, Check)], json: bool) -> Result<()> {
    if json {
        let entries = checks
            .iter()
            .map(|(name, check)| {
                (
                    (*name).to_owned(),
                    json!({ "status": check.status.as_str(), "message": check.message }),
                )
            })
            .collect::<serde_json::Map<_, _>>();

        print_json(json!({ "checks": entries }))?;
    } else {
        for (name, check) in checks {
            println!("{name}: {} {}", check.status.as_str(), check.message);
        }
    }

    let failures = checks
        .iter()
        .filter(|(_, check)| check.status == Status::Fail)
        .count();

    if failures > 0 {
        anyhow::bail!("{failures} check(s) failed");
    }

    Ok(())
}

fn sunshine_pairing_check<E>(credentials: &Result<Option<impl Sized>, E>) -> Check {
    match credentials {
        Ok(Some(_)) => Check::pass("Sunshine admin credentials configured on this host"),
        Ok(None) => Check::warn(
            "Sunshine admin credentials missing; run `omdesky setup` on this host to enable pairing",
        ),
        Err(_) => Check::fail("The desktop Secret Service is unavailable or locked"),
    }
}

fn agent_check(health: Option<&HealthResponse>, port: u16) -> Check {
    let Some(health) = health else {
        return Check::fail(format!(
            "omdesky-agent is not reachable on port {port}; run `systemctl --user start {AGENT_UNIT}`"
        ));
    };

    if ensure_compatible_agent(health).is_err() {
        return Check::fail(format!(
            "omdesky-agent ({}, protocol {}) does not match this command ({RELEASE}, protocol {PROTOCOL}); restart it with `systemctl --user restart {AGENT_UNIT}`",
            health.agent_version, health.protocol
        ));
    }

    Check::pass(format!("omdesky-agent {} is running", health.agent_version))
}

fn agent_service_check(service: Option<&Path>, current_exe: Option<&Path>) -> Check {
    let Some(service) = service else {
        return Check::warn(format!(
            "{AGENT_UNIT} is not installed; the agent must run as a user service"
        ));
    };

    let expected = current_exe
        .and_then(Path::parent)
        .map(|directory| directory.join("omdesky-agent"));

    let same_install = expected
        .as_deref()
        .is_some_and(|expected| canonical(expected) == canonical(service));

    if same_install {
        return Check::pass(format!("{AGENT_UNIT} runs {}", service.display()));
    }

    Check::warn(format!(
        "{AGENT_UNIT} runs {} but this command lives in {}; both should come from the same installation",
        service.display(),
        current_exe.and_then(Path::parent).map_or_else(
            || "an unknown directory".to_owned(),
            |path| path.display().to_string()
        )
    ))
}

fn canonical(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn access_check(allowlist: Option<&[AllowedController]>) -> Check {
    let Some(allowlist) = allowlist else {
        return Check::fail("the allowed devices list could not be read");
    };

    if allowlist.is_empty() {
        return Check::warn(
            "no computer is allowed yet; run `omdesky access allow <device>` on both computers",
        );
    }

    let inert = allowlist
        .iter()
        .filter(|entry| entry.capabilities.is_empty())
        .map(|entry| {
            entry
                .label
                .clone()
                .unwrap_or_else(|| entry.tailnet_node_id.clone())
        })
        .collect::<Vec<_>>();

    if !inert.is_empty() {
        return Check::warn(format!(
            "{} grant nothing, probably written by an earlier release; run `omdesky access allow` again for them",
            inert.join(", ")
        ));
    }

    Check::pass(format!("{} computer(s) allowed", allowlist.len()))
}

fn devices_check<E: std::fmt::Display>(devices: Result<&[DiscoveredNode], E>) -> Check {
    let devices = match devices {
        Ok(devices) => devices,
        Err(error) => return Check::fail(error.to_string()),
    };

    let local_problems = devices
        .iter()
        .filter(|node| node.is_local)
        .flat_map(|node| {
            node.blockers
                .iter()
                .map(|blocker| blocker_message(&node.name, blocker))
        });

    let remote = devices
        .iter()
        .filter(|node| !node.is_local)
        .collect::<Vec<_>>();

    let ready = remote
        .iter()
        .filter(|node| node.status == NodeStatus::Ready)
        .count();

    let problems = local_problems
        .chain(remote.iter().flat_map(|node| {
            node.blockers
                .iter()
                .map(|blocker| blocker_message(&node.name, blocker))
        }))
        .collect::<Vec<_>>();

    if !problems.is_empty() {
        return Check::warn(problems.join(" "));
    }

    if remote.is_empty() {
        return Check::warn("no other Omdesky device is online");
    }

    Check::pass(format!("{ready} of {} device(s) ready", remote.len()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_core::{BlockerCode, BlockerSide, ConnectionKind, NodeBlocker, NodeCapabilities};
    use std::net::{IpAddr, Ipv4Addr};
    use time::OffsetDateTime;

    fn health(agent_version: &str) -> HealthResponse {
        HealthResponse {
            status: "ok".to_owned(),
            protocol: PROTOCOL,
            agent_version: agent_version.to_owned(),
        }
    }

    fn node(name: &str, blockers: Vec<NodeBlocker>) -> DiscoveredNode {
        DiscoveredNode {
            tailnet_node_id: format!("n{name}"),
            name: name.to_owned(),
            address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)),
            status: if blockers.is_empty() {
                NodeStatus::Ready
            } else {
                NodeStatus::Blocked
            },
            blockers,
            connection: ConnectionKind::Direct,
            latency_ms: Some(3),
            agent_version: None,
            omarchy_version: None,
            capabilities: NodeCapabilities::default(),
            is_local: false,
        }
    }

    #[test]
    fn test_a_missing_agent_fails() {
        assert_eq!(agent_check(None, 48155).status, Status::Fail);
    }

    #[test]
    fn test_an_agent_from_another_release_fails_with_a_restart_hint() {
        let check = agent_check(Some(&health("99.0.0")), 48155);

        assert_eq!(check.status, Status::Fail);
        assert!(check.message.contains("systemctl --user restart"));
    }

    #[test]
    fn test_a_matching_agent_passes() {
        assert_eq!(
            agent_check(Some(&health(RELEASE)), 48155).status,
            Status::Pass
        );
    }

    #[test]
    fn test_a_service_from_another_installation_warns() {
        let check = agent_service_check(
            Some(Path::new("/usr/bin/omdesky-agent")),
            Some(Path::new("/home/user/.local/bin/omdesky")),
        );

        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("/usr/bin/omdesky-agent"));
    }

    #[test]
    fn test_a_service_from_the_same_installation_passes() {
        let check = agent_service_check(
            Some(Path::new("/opt/omdesky/omdesky-agent")),
            Some(Path::new("/opt/omdesky/omdesky")),
        );

        assert_eq!(check.status, Status::Pass);
    }

    #[test]
    fn test_a_missing_service_warns() {
        assert_eq!(
            agent_service_check(None, Some(Path::new("/usr/bin/omdesky"))).status,
            Status::Warn
        );
    }

    #[test]
    fn test_an_empty_allowlist_warns() {
        assert_eq!(access_check(Some(&[])).status, Status::Warn);
    }

    #[test]
    fn test_entries_without_capabilities_are_named() {
        let entry = AllowedController::new(
            "nOLD",
            Some("desk-old".to_owned()),
            OffsetDateTime::UNIX_EPOCH,
            [],
        );

        let check = access_check(Some(&[entry]));

        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("desk-old"));
    }

    #[test]
    fn test_blocked_devices_are_reported_with_their_fix() {
        let blocker = NodeBlocker {
            code: BlockerCode::NeedsAccess,
            side: BlockerSide::Local,
            fix: "omdesky access allow desk-b".to_owned(),
        };
        let devices = [node("desk-b", vec![blocker]), node("desk-c", Vec::new())];

        let check = devices_check::<String>(Ok(&devices));

        assert_eq!(check.status, Status::Warn);
        assert!(check.message.contains("omdesky access allow desk-b"));
    }

    #[test]
    fn test_ready_devices_pass() {
        let devices = [node("desk-b", Vec::new())];

        let check = devices_check::<String>(Ok(&devices));

        assert_eq!(check, Check::pass("1 of 1 device(s) ready"));
    }
}
