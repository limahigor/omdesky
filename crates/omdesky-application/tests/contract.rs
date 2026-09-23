use omdesky_application::discovery::DiscoveredNode;
use omdesky_core::{
    BlockerCode, BlockerSide, ConnectionKind, Display, DisplayId, KeyChord, KeyModifier,
    NodeBlocker, NodeCapabilities, NodeStatus, RemoteCommand, SessionClaim, SessionEndpoint,
    SessionGrant, SessionId, SessionRole, Window, WindowId, WindowSelector, Workspace, WorkspaceId,
    WorkspaceTarget,
};
use omdesky_protocol::{
    ActiveWindowResponse, CLI_SCHEMA, CapabilitiesResponse, CommandRequest, CommandResponse,
    DisplaysResponse, ErrorCode, ErrorEnvelope, FocusResponse, FocusWorkspaceRequest,
    HealthResponse, NodeInfoResponse, PROTOCOL, ReleaseLine, SunshinePairChallengeResponse,
    SunshinePairRequest, SunshinePairResponse, SunshineStatusResponse, WindowsResponse,
    WorkspacesResponse,
};
use serde::{Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::{
    fs,
    net::{IpAddr, Ipv4Addr},
    path::{Path, PathBuf},
    sync::Mutex,
};
use time::OffsetDateTime;
use uuid::Uuid;

const UPDATE_VARIABLE: &str = "OMDESKY_UPDATE_CONTRACT";
const LOCK_FILE: &str = "contract.lock";

static FIXTURES: Mutex<()> = Mutex::new(());

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/contract")
}

fn updating() -> bool {
    std::env::var_os(UPDATE_VARIABLE).is_some()
}

fn record(name: &str, encoded: &Value) {
    let path = fixtures().join(format!("{name}.json"));

    if updating() {
        fs::create_dir_all(fixtures()).expect("fixture directory is writable");

        let mut contents = serde_json::to_string_pretty(encoded).expect("fixture serializes");
        contents.push('\n');

        fs::write(&path, contents).expect("fixture is writable");
        return;
    }

    let contents = fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("{name}.json is missing; run {UPDATE_VARIABLE}=1 cargo test -p omdesky-application --test contract")
    });
    let fixture: Value = serde_json::from_str(&contents).expect("fixture is valid JSON");

    assert_eq!(
        encoded, &fixture,
        "the {name} wire format changed; bump the minor version and run {UPDATE_VARIABLE}=1 cargo test -p omdesky-application --test contract"
    );
}

fn assert_contract<T>(name: &str, value: &T)
where
    T: Serialize + DeserializeOwned,
{
    let _guard = FIXTURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let encoded = serde_json::to_value(value).expect("contract value serializes");

    record(name, &encoded);

    let decoded: T = serde_json::from_value(encoded.clone()).expect("fixture deserializes");
    let reencoded = serde_json::to_value(&decoded).expect("decoded value serializes");

    assert_eq!(reencoded, encoded, "{name} does not round-trip");
}

fn session_id() -> SessionId {
    "6f1c1b9e-8a3f-4b0e-9f5e-2d7c3a1b4e5f"
        .parse()
        .expect("fixed session identifier")
}

fn issued_at() -> OffsetDateTime {
    OffsetDateTime::from_unix_timestamp(1_790_000_000).expect("fixed timestamp")
}

fn command(command: RemoteCommand, session: Option<SessionClaim>) -> CommandRequest {
    CommandRequest {
        command,
        session,
        request_id: Uuid::parse_str("0b6f7c2e-5d1a-4c3b-9e8f-7a6b5c4d3e2f").expect("fixed id"),
        issued_at: issued_at(),
    }
}

fn window() -> Window {
    Window {
        id: WindowId("0x55aa".to_owned()),
        app_id: Some("firefox".to_owned()),
        class: Some("firefox".to_owned()),
        title: Some("Omdesky".to_owned()),
        workspace: WorkspaceId(2),
        focused: true,
    }
}

#[test]
fn test_health_contract() {
    assert_contract(
        "health",
        &HealthResponse {
            status: "ok".to_owned(),
            protocol: PROTOCOL,
            agent_version: "0.2.0".to_owned(),
        },
    );
}

#[test]
fn test_node_info_contract() {
    assert_contract(
        "node-info",
        &NodeInfoResponse {
            node_id: session_id().to_string(),
            hostname: "desk-b".to_owned(),
            omarchy_version: "4.0.1".to_owned(),
            agent_version: "0.2.0".to_owned(),
            capabilities: vec!["desktop.stream-host".to_owned()],
        },
    );
}

#[test]
fn test_desktop_query_contracts() {
    assert_contract(
        "displays",
        &DisplaysResponse {
            displays: vec![Display {
                id: "DP-1".to_owned(),
                name: "Main".to_owned(),
                width: 2560,
                height: 1440,
                refresh_hz: 144.0,
                focused: true,
            }],
        },
    );
    assert_contract(
        "workspaces",
        &WorkspacesResponse {
            workspaces: vec![Workspace {
                id: WorkspaceId(2),
                name: Some("code".to_owned()),
                monitor: Some("DP-1".to_owned()),
                focused: true,
                windows: 3,
            }],
        },
    );
    assert_contract(
        "windows",
        &WindowsResponse {
            windows: vec![window()],
        },
    );
    assert_contract(
        "active-window",
        &ActiveWindowResponse {
            window: Some(window()),
        },
    );
}

#[test]
fn test_focus_contracts() {
    assert_contract(
        "focus-workspace-id",
        &FocusWorkspaceRequest {
            target: WorkspaceTarget::Id(WorkspaceId(2)),
        },
    );
    assert_contract(
        "focus-workspace-name",
        &FocusWorkspaceRequest {
            target: WorkspaceTarget::Name("code".to_owned()),
        },
    );
    assert_contract("focus-response", &FocusResponse { focused: true });
}

#[test]
fn test_command_contracts() {
    let chord = KeyChord::new([KeyModifier::Super], "R").expect("valid chord");
    let claim = SessionClaim {
        id: session_id(),
        generation: 3,
    };

    assert_contract(
        "command-send-shortcut",
        &command(
            RemoteCommand::SendShortcut {
                chord,
                window: WindowSelector::Class("com.moonlight_stream.Moonlight".to_owned()),
            },
            Some(claim),
        ),
    );
    assert_contract(
        "command-close-window",
        &command(
            RemoteCommand::CloseWindow {
                window: WindowSelector::Address("0x55aa".to_owned()),
            },
            Some(claim),
        ),
    );
    assert_contract(
        "command-switch-stream-display",
        &command(
            RemoteCommand::SwitchStreamDisplay {
                display: DisplayId::from("DP-2"),
            },
            Some(claim),
        ),
    );
    assert_contract(
        "command-attach-session",
        &command(
            RemoteCommand::AttachSession {
                role: SessionRole::Remote,
                controller: Some(SessionEndpoint {
                    address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7)),
                    port: 48155,
                }),
                window: Some(WindowSelector::ActiveWindow),
            },
            None,
        ),
    );
    assert_contract(
        "command-detach-session",
        &command(RemoteCommand::DetachSession, Some(claim)),
    );
    assert_contract(
        "command-renew-session",
        &command(RemoteCommand::RenewSession, Some(claim)),
    );
    assert_contract(
        "command-response-granted",
        &CommandResponse::granted(SessionGrant {
            id: session_id(),
            generation: 3,
            lease_seconds: 30,
        }),
    );
    assert_contract("command-response-ignored", &CommandResponse::ignored());
}

#[test]
fn test_sunshine_contracts() {
    assert_contract(
        "sunshine-status",
        &SunshineStatusResponse {
            installed: true,
            running: true,
            ready: true,
            desktop_app: Some("Desktop".to_owned()),
            pairing_api_available: true,
        },
    );
    assert_contract(
        "sunshine-pair-challenge",
        &SunshinePairChallengeResponse {
            pairing_id: "ab-12".to_owned(),
            expires_in_seconds: 120,
        },
    );
    assert_contract(
        "sunshine-pair-request",
        &SunshinePairRequest {
            pairing_id: "ab-12".to_owned(),
            pin: "1234".to_owned(),
            client_name: "desk-a".to_owned(),
        },
    );
    assert_contract(
        "sunshine-pair-response",
        &SunshinePairResponse { paired: true },
    );
}

#[test]
fn test_access_contracts() {
    assert_contract(
        "capabilities",
        &CapabilitiesResponse {
            capabilities: omdesky_core::ControlCapability::ALL.to_vec(),
        },
    );
    assert_contract(
        "error-envelope",
        &ErrorEnvelope::new(ErrorCode::SessionNotOwned, "This device does not own it."),
    );

    let vocabulary = ErrorCode::ALL
        .iter()
        .map(|code| {
            json!({
                "code": code.as_str(),
                "status": code.http_status(),
                "retryable": code.retryable(),
            })
        })
        .collect::<Vec<_>>();

    let _guard = FIXTURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    record("error-codes", &Value::Array(vocabulary));
}

#[test]
fn test_cli_devices_contract() {
    let node = DiscoveredNode {
        tailnet_node_id: "nREMOTE".to_owned(),
        name: "desk-b".to_owned(),
        address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)),
        status: NodeStatus::Blocked,
        blockers: vec![NodeBlocker {
            code: BlockerCode::NeedsAccess,
            side: BlockerSide::Local,
            fix: "omdesky access allow desk-b".to_owned(),
        }],
        connection: ConnectionKind::Direct,
        latency_ms: Some(4),
        agent_version: Some("0.2.0".to_owned()),
        omarchy_version: Some("4.0.1".to_owned()),
        capabilities: NodeCapabilities::new(["desktop.stream-host".to_owned()]),
        is_local: false,
    };

    let document = json!({ "schema": CLI_SCHEMA, "devices": [node] });

    let _guard = FIXTURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    record("cli-devices", &document);
}

fn digest(directory: &Path) -> u64 {
    let mut names = fs::read_dir(directory)
        .expect("fixture directory is readable")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.ends_with(".json"))
        .collect::<Vec<_>>();

    names.sort();

    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;

    for name in names {
        let contents = fs::read(directory.join(&name)).expect("fixture is readable");

        for byte in name.bytes().chain([0]).chain(contents).chain([0]) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }

    hash
}

fn current_line() -> String {
    let line = ReleaseLine::current().expect("current release parses");

    format!("{}.{}", line.major, line.minor)
}

fn parse_lock(contents: &str) -> Option<(String, String)> {
    let mut line = None;
    let mut digest = None;

    for entry in contents.lines() {
        let (key, value) = entry.split_once('=')?;

        match key.trim() {
            "line" => line = Some(value.trim().to_owned()),
            "digest" => digest = Some(value.trim().to_owned()),
            _ => return None,
        }
    }

    Some((line?, digest?))
}

#[test]
fn test_contract_changes_require_a_new_release_line() {
    test_health_contract();
    test_node_info_contract();
    test_desktop_query_contracts();
    test_focus_contracts();
    test_command_contracts();
    test_sunshine_contracts();
    test_access_contracts();
    test_cli_devices_contract();

    let _guard = FIXTURES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());

    let directory = fixtures();
    let line = current_line();
    let digest = format!("{:016x}", digest(&directory));

    let lock_path = directory.join(LOCK_FILE);
    let recorded = fs::read_to_string(&lock_path)
        .ok()
        .and_then(|contents| parse_lock(&contents));

    if updating() {
        if let Some((recorded_line, recorded_digest)) = &recorded
            && recorded_line == &line
            && recorded_digest != &digest
        {
            panic!(
                "the wire contract changed inside release line {line}; bump the minor version before recording it"
            );
        }

        fs::write(&lock_path, format!("line = {line}\ndigest = {digest}\n"))
            .expect("contract lock is writable");
        return;
    }

    let Some((recorded_line, recorded_digest)) = recorded else {
        panic!(
            "{LOCK_FILE} is missing or malformed; run {UPDATE_VARIABLE}=1 cargo test -p omdesky-application --test contract"
        );
    };

    assert_eq!(
        recorded_line, line,
        "the release line is now {line}; record the contract with {UPDATE_VARIABLE}=1 cargo test -p omdesky-application --test contract"
    );
    assert_eq!(
        recorded_digest, digest,
        "the wire contract changed inside release line {line}; bump the minor version and record it again"
    );
}
