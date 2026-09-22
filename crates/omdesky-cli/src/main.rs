#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use omdesky_application::{
    ports::{
        AccessStore, AgentClient, AgentEndpoint, AllowedController, LauncherSpec, LauncherStore,
        MeshNetwork, StreamHost,
    },
    services::{ConnectNode, ConnectRequest, DiscoverNodes, PairStream, ensure_controller_ready},
};
use omdesky_core::{
    CodecPreference, ControlCapability, DisplayId, InputMode, KeyChord, KeyModifier, NodeId,
    RemoteCommand, StreamProfile, WindowSelector, WorkspaceTarget, bitrate_kbps_from_mbps,
    text::{DISPLAY_NAME_LIMIT, sanitize_for_display},
};
use omdesky_platform::{
    access::FileAccessStore,
    agent_client::HttpAgentClient,
    config::{
        Config, access_path, legacy_sunshine_credentials_path, runtime_session_path, state_dir,
        sunshine_config_path,
    },
    hyprland::HyprlandAdapter,
    input::HyprlandSessionKeybinds,
    launcher::DesktopLauncherStore,
    moonlight::MoonlightAdapter,
    omarchy::{OmarchyNotificationAdapter, detect_version},
    process::TokioCommandRunner,
    session_record::{SessionRecord, SupervisedProcess},
    sunshine::{SunshineAdapter, SunshineCredentialStore},
    tailscale::TailscaleAdapter,
};
use omdesky_protocol::{CommandRequest, SunshinePairRequest};
use serde_json::json;
use std::{env, net::IpAddr, path::PathBuf, str::FromStr, sync::Arc};
use time::OffsetDateTime;
#[cfg(debug_assertions)]
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "omdesky", version, about = "Omdesky")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    Devices {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        all_tailnet: bool,
    },

    Info {
        target: String,
        #[arg(long)]
        json: bool,
    },

    Displays {
        target: String,
        #[arg(long)]
        json: bool,
    },

    Workspaces {
        target: String,
        #[arg(long)]
        json: bool,
    },

    Windows {
        target: String,
        #[arg(long)]
        workspace: Option<i64>,
        #[arg(long)]
        app_id: Option<String>,
        #[arg(long)]
        json: bool,
    },

    Pair {
        target: String,
    },

    SunshinePin {
        pin: String,
        #[arg(long, default_value = "moonlight")]
        name: String,
    },

    Connect(ConnectArgs),

    #[command(name = "command")]
    RemoteCmd {
        #[command(subcommand)]
        action: CommandAction,
    },

    Input {
        #[command(subcommand)]
        command: InputCommand,
    },

    Session {
        #[arg(long)]
        json: bool,
    },

    Disconnect,

    Access {
        #[command(subcommand)]
        command: AccessCommand,
    },

    Doctor {
        #[arg(long)]
        json: bool,
    },

    Launcher {
        #[command(subcommand)]
        command: LauncherCommand,
    },

    Setup,
}

#[derive(Args)]
struct ConnectArgs {
    target: String,
    #[arg(long)]
    workspace: Option<String>,
    #[arg(long)]
    window: Option<String>,
    #[arg(long)]
    app_id: Option<String>,
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 1080)]
    height: u32,
    #[arg(long, default_value_t = 60)]
    fps: u16,
    #[arg(long, value_enum, default_value_t = CodecArg::Auto)]
    codec: CodecArg,

    #[arg(long)]
    bitrate: Option<u32>,
    #[arg(long, conflicts_with = "windowed")]
    fullscreen: bool,
    #[arg(long)]
    windowed: bool,
    #[arg(long, value_enum, default_value_t = InputArg::Remote)]
    input: InputArg,
    #[arg(long)]
    no_audio: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Clone, Copy, ValueEnum)]
enum CodecArg {
    Auto,
    H264,
    Hevc,
    Av1,
}

#[derive(Clone, Copy, ValueEnum)]
enum InputArg {
    Local,
    Remote,
}

#[derive(Subcommand)]
enum InputCommand {
    Status,
    Local,
    Remote,
    Toggle,
}

#[derive(Subcommand)]
enum CommandAction {
    SendShortcut {
        target: String,

        #[arg(long)]
        mods: String,

        #[arg(long)]
        key: String,

        #[arg(long, conflicts_with = "window_address")]
        window_class: Option<String>,

        #[arg(long)]
        window_address: Option<String>,

        #[arg(long)]
        port: Option<u16>,
    },
    CloseWindow {
        target: String,

        #[arg(long, conflicts_with = "window_address")]
        window_class: Option<String>,

        #[arg(long)]
        window_address: Option<String>,

        #[arg(long)]
        port: Option<u16>,
    },
    SwitchDisplay {
        target: String,

        #[arg(long)]
        display: String,

        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Subcommand)]
enum AccessCommand {
    List,
    Allow {
        peer: String,

        #[arg(long = "capability", value_name = "CAPABILITY")]
        capabilities: Vec<String>,
    },
    Revoke {
        peer: String,
    },
}

#[derive(Subcommand)]
enum LauncherCommand {
    Create { target: String },
    List,
    Remove { target: String },
}

#[cfg(debug_assertions)]
fn init_debug_tracing() {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
}

#[cfg(not(debug_assertions))]
fn init_debug_tracing() {}

#[tokio::main]
async fn main() -> Result<()> {
    init_debug_tracing();

    let cli = Cli::parse();

    match cli.command {
        None => omdesky_tui::run().await,
        Some(Command::Devices { json, all_tailnet }) => devices(json, all_tailnet).await,
        Some(Command::Info { target, json }) => info(&target, json).await,
        Some(Command::Displays { target, json }) => displays(&target, json).await,
        Some(Command::Workspaces { target, json }) => workspaces(&target, json).await,
        Some(Command::Windows {
            target,
            workspace,
            app_id,
            json,
        }) => windows(&target, workspace, app_id, json).await,
        Some(Command::Pair { target }) => pair(&target).await,
        Some(Command::SunshinePin { pin, name }) => sunshine_pin(&pin, &name).await,
        Some(Command::Connect(args)) => connect(args).await,
        Some(Command::RemoteCmd { action }) => remote_command(action).await,
        Some(Command::Input { command }) => input(command).await,
        Some(Command::Session { json }) => session(json).await,
        Some(Command::Disconnect) => disconnect().await,
        Some(Command::Access { command }) => access(command).await,
        Some(Command::Doctor { json }) => doctor(json).await,
        Some(Command::Launcher { command }) => launcher(command).await,
        Some(Command::Setup) => setup().await,
    }
}

fn agent_client() -> Arc<HttpAgentClient> {
    Arc::new(HttpAgentClient::new())
}

fn sunshine_credential_store() -> Result<SunshineCredentialStore> {
    Ok(SunshineCredentialStore::new(Some(
        legacy_sunshine_credentials_path()?,
    )))
}

async fn devices(json: bool, all_tailnet: bool) -> Result<()> {
    let config = Config::load()?;
    let runner = Arc::new(TokioCommandRunner);

    let discovery = DiscoverNodes::new(
        Arc::new(TailscaleAdapter::new(runner)),
        agent_client(),
        config.network.agent_port,
    );
    let nodes = discovery.execute(all_tailnet).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&nodes)?);
    } else {
        println!("NAME\tSTATUS\tLINK\tLATENCY");
        for node in nodes {
            println!(
                "{}\t{:?}\t{:?}\t{}",
                node.name,
                node.status,
                node.connection,
                node.latency_ms
                    .map_or_else(|| "—".to_owned(), |value| format!("{value}ms"))
            );
        }
    }

    Ok(())
}

async fn info(target: &str, json: bool) -> Result<()> {
    let endpoint = resolve_endpoint(target).await?;
    let client = HttpAgentClient::new();

    let node = client.node_info(&endpoint).await?;
    let displays = client.displays(&endpoint).await?;

    if json {
        println!("{}", json!({"node": node, "displays": displays}));
    } else {
        println!("{} ({})", node.hostname, node.node_id);
        println!(
            "Omarchy {}  ·  agent {}",
            node.omarchy_version, node.agent_version
        );
        for display in displays {
            println!(
                "{} {}x{}@{}",
                display.id, display.width, display.height, display.refresh_hz
            );
        }
    }

    Ok(())
}

async fn displays(target: &str, json: bool) -> Result<()> {
    let endpoint = resolve_endpoint(target).await?;
    let displays = HttpAgentClient::new().displays(&endpoint).await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&displays)?);
    } else {
        for display in displays {
            let marker = if display.focused { "●" } else { " " };
            println!(
                "{marker} {}\t{}x{}@{}",
                display.id, display.width, display.height, display.refresh_hz
            );
        }
    }

    Ok(())
}

async fn workspaces(target: &str, json: bool) -> Result<()> {
    let endpoint = resolve_endpoint(target).await?;
    let client = HttpAgentClient::new();

    let workspaces = client.workspaces(&endpoint).await?;
    let windows = client.windows(&endpoint).await.unwrap_or_default();

    if json {
        println!("{}", serde_json::to_string_pretty(&workspaces)?);
    } else {
        for workspace in workspaces {
            let name = workspace.name.as_deref().unwrap_or("—");
            println!("{}  {name}", workspace.id);
            for window in windows.iter().filter(|w| w.workspace == workspace.id) {
                let label = window
                    .title
                    .as_deref()
                    .or(window.app_id.as_deref())
                    .or(window.class.as_deref())
                    .unwrap_or("—");
                println!("   {label}");
            }
        }
    }

    Ok(())
}

async fn windows(
    target: &str,
    workspace: Option<i64>,
    app_id: Option<String>,
    json: bool,
) -> Result<()> {
    let endpoint = resolve_endpoint(target).await?;

    let windows = HttpAgentClient::new()
        .windows(&endpoint)
        .await?
        .into_iter()
        .filter(|window| workspace.is_none_or(|id| window.workspace.0 == id))
        .filter(|window| {
            app_id.as_deref().is_none_or(|needle| {
                window.app_id.as_deref() == Some(needle) || window.class.as_deref() == Some(needle)
            })
        })
        .collect::<Vec<_>>();

    if json {
        println!("{}", serde_json::to_string_pretty(&windows)?);
    } else {
        println!("WORKSPACE\tAPP\tTITLE");
        for window in windows {
            println!(
                "{}\t{}\t{}",
                window.workspace,
                window.app_id.or(window.class).as_deref().unwrap_or("—"),
                window.title.as_deref().unwrap_or("—")
            );
        }
    }

    Ok(())
}

async fn pair(target: &str) -> Result<()> {
    let endpoint = resolve_endpoint(target).await?;
    let service = PairStream::new(agent_client(), moonlight_adapter(), client_name());

    service.execute(&endpoint).await?;
    println!("Paired Moonlight with {target}");

    Ok(())
}

async fn sunshine_pin(pin: &str, name: &str) -> Result<()> {
    let runner = Arc::new(TokioCommandRunner);

    let adapter = SunshineAdapter::with_api(
        runner,
        sunshine_config_path()?,
        omdesky_platform::sunshine::DEFAULT_API_BASE.to_owned(),
        sunshine_credential_store()?,
    );

    adapter
        .submit_pairing_pin(SunshinePairRequest {
            pairing_id: None,
            pin: pin.to_owned(),
            client_name: name.to_owned(),
        })
        .await?;

    println!("Submitted PIN to local Sunshine; Moonlight should finish pairing");

    Ok(())
}

async fn connect(args: ConnectArgs) -> Result<()> {
    let config = Config::load()?;
    let client = agent_client();
    let controller_endpoint = local_agent_endpoint(config.network.agent_port)
        .await
        .map_err(|error| {
            anyhow::anyhow!("could not resolve this controller's Tailscale endpoint: {error}")
        })?;
    let local_agent_available = client.health(&controller_endpoint).await.is_ok();
    let sunshine_configured = sunshine_credential_store()?.configured().await?;
    ensure_controller_ready(local_agent_available, sunshine_configured)?;

    let endpoint = resolve_endpoint(&args.target).await?;
    let notifications = Arc::new(OmarchyNotificationAdapter::default());
    let service = ConnectNode::new(
        client,
        moonlight_adapter(),
        Arc::new(HyprlandSessionKeybinds::new(Arc::new(TokioCommandRunner))),
        notifications,
        client_name(),
    );

    let input_mode = match args.input {
        InputArg::Local => InputMode::Local,
        InputArg::Remote => InputMode::Remote,
    };
    let focus_workspace = args.workspace.as_deref().map(parse_workspace_target);

    let bitrate_mbps = args
        .bitrate
        .or_else(|| (config.stream.bitrate_mbps > 0).then_some(config.stream.bitrate_mbps));
    let bitrate_kbps = bitrate_mbps
        .map(bitrate_kbps_from_mbps)
        .transpose()
        .context("invalid stream bitrate")?;

    let profile = StreamProfile {
        width: args.width,
        height: args.height,
        fps: args.fps,
        codec_preference: match args.codec {
            CodecArg::Auto => CodecPreference::Auto,
            CodecArg::H264 => CodecPreference::H264,
            CodecArg::Hevc => CodecPreference::Hevc,
            CodecArg::Av1 => CodecPreference::Av1,
        },
        audio: !args.no_audio,
        bitrate_kbps,
    };
    profile.validate().context("invalid stream profile")?;

    let session_path = runtime_session_path().ok();
    let mut record = SessionRecord::new(&args.target, input_mode);

    if let Some(path) = &session_path
        && let Err(error) = record.write(path)
    {
        tracing::debug!(detail = %error, "session.record_write_failed");
    }

    let record_path = session_path.clone();
    let session_id = record.session_id;

    let exit = service
        .execute_with_started(
            ConnectRequest {
                endpoint: endpoint.clone(),
                controller_endpoint,
                profile,
                fullscreen: args.fullscreen || !args.windowed,
                input_mode,
                focus_workspace,
                focus_window: args.window.or(args.app_id),
                auto_pair: true,
            },
            move |moonlight_pid| {
                let record_path = record_path.clone();

                async move {
                    let Some(path) = record_path else {
                        return;
                    };

                    record.moonlight = moonlight_pid.and_then(SupervisedProcess::observe);

                    if let Err(error) = record.write(&path) {
                        tracing::debug!(detail = %error, "session.record_update_failed");
                    }
                }
            },
        )
        .await;

    if let Some(path) = &session_path {
        SessionRecord::remove_if_owned(path, session_id);
    }

    let exit = exit?;

    if args.json {
        println!("{}", json!({"exit_status": exit}));
    }

    Ok(())
}

async fn local_agent_endpoint(port: u16) -> Result<AgentEndpoint> {
    let local = TailscaleAdapter::new(Arc::new(TokioCommandRunner))
        .local_node()
        .await?;
    let address = local
        .addresses
        .iter()
        .find(|address| address.is_ipv4())
        .or(local.addresses.first())
        .copied()
        .context("Tailscale has no local address for the session keybindings")?;

    Ok(AgentEndpoint { address, port })
}

async fn input(command: InputCommand) -> Result<()> {
    match command {
        InputCommand::Status => {
            let mode = read_session().map_or("local", |record| match record.input_mode {
                InputMode::Local => "local",
                InputMode::Remote => "remote",
            });
            println!("input mode: {mode}");
            Ok(())
        }
        InputCommand::Local | InputCommand::Remote | InputCommand::Toggle => anyhow::bail!(
            "live input switching is not supported by the installed Moonlight; set --input at connect time"
        ),
    }
}

async fn session(json: bool) -> Result<()> {
    match read_session() {
        Some(record) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&record)?);
            } else {
                println!(
                    "node {}  ·  pid {}  ·  input {}",
                    sanitize_for_display(&record.remote_node, DISPLAY_NAME_LIMIT),
                    record
                        .moonlight
                        .map_or_else(|| "—".to_owned(), |process| process.pid.to_string()),
                    match record.input_mode {
                        InputMode::Local => "local",
                        InputMode::Remote => "remote",
                    }
                );
            }
            Ok(())
        }
        None => {
            if json {
                println!("null");
                Ok(())
            } else {
                anyhow::bail!("no active Omdesky session")
            }
        }
    }
}

async fn disconnect() -> Result<()> {
    let path = runtime_session_path()?;
    let record = SessionRecord::read(&path).context("no active Omdesky session to disconnect")?;

    match record.moonlight {
        Some(process) if process.is_still_running() => {
            terminate_process(process.pid);
            println!("Signalled Moonlight process {} to stop", process.pid);
        }
        Some(process) => {
            println!(
                "Moonlight process {} is no longer the one this session started; nothing was signalled",
                process.pid
            );
        }
        None => println!("Session has no supervised Moonlight process"),
    }

    SessionRecord::remove_if_owned(&path, record.session_id);

    Ok(())
}

async fn access(command: AccessCommand) -> Result<()> {
    let store = FileAccessStore::new(access_path()?);
    match command {
        AccessCommand::List => {
            for entry in store.list().await? {
                let capabilities = entry
                    .capabilities
                    .iter()
                    .map(|capability| capability.as_str())
                    .collect::<Vec<_>>()
                    .join(",");

                println!(
                    "{}\t{}\t{}",
                    sanitize_for_display(&entry.tailnet_node_id, DISPLAY_NAME_LIMIT),
                    sanitize_for_display(entry.label.as_deref().unwrap_or("—"), DISPLAY_NAME_LIMIT),
                    capabilities
                );
            }
        }
        AccessCommand::Allow {
            peer,
            capabilities: requested,
        } => {
            let capabilities = parse_capabilities(&requested)?;
            let identity = resolve_tailnet_identity(&peer).await?;

            store
                .allow(AllowedController::new(
                    identity,
                    Some(peer.clone()),
                    OffsetDateTime::now_utc(),
                    capabilities,
                ))
                .await?;

            println!("Allowed {peer}");
        }
        AccessCommand::Revoke { peer } => {
            let identity = resolve_tailnet_identity(&peer)
                .await
                .unwrap_or_else(|_| peer.clone());
            store.revoke(&identity).await?;
            println!("Revoked {peer}");
        }
    }

    Ok(())
}

async fn doctor(json: bool) -> Result<()> {
    let runner = Arc::new(TokioCommandRunner);

    let omarchy = detect_version(runner.as_ref()).await;
    let tailscale = TailscaleAdapter::new(runner.clone()).local_node().await;
    let hyprland = HyprlandAdapter::new(runner.clone()).displays_probe().await;
    let sunshine = SunshineAdapter::with_api(
        runner,
        sunshine_config_path()?,
        omdesky_platform::sunshine::DEFAULT_API_BASE.to_owned(),
        sunshine_credential_store()?,
    )
    .readiness()
    .await;

    let credential_store = sunshine_credential_store()?;
    let credential_status = tokio::task::spawn_blocking(move || credential_store.load())
        .await
        .context("query desktop Secret Service")?;
    let sunshine_pairing = match credential_status {
        Ok(Some(_)) => {
            json!({"status": "PASS", "message": "Sunshine admin credentials configured on this host"})
        }
        Ok(None) => json!({
            "status": "WARN",
            "message": "Sunshine admin credentials missing; run `omdesky setup` on this host to enable pairing"
        }),
        Err(_) => json!({
            "status": "FAIL",
            "message": "The desktop Secret Service is unavailable or locked"
        }),
    };

    let report = json!({
        "omarchy": check(&omarchy),
        "tailscale": check(&tailscale),
        "hyprland": check(&hyprland),
        "sunshine": check(&sunshine),
        "sunshine_pairing": sunshine_pairing
    });

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for (name, value) in report.as_object().expect("object") {
            println!(
                "{name}: {} {}",
                value["status"].as_str().unwrap_or("FAIL"),
                value["message"].as_str().unwrap_or("")
            );
        }
    }

    Ok(())
}

fn check<T, E: std::fmt::Display>(result: &Result<T, E>) -> serde_json::Value {
    match result {
        Ok(_) => json!({"status": "PASS", "message": "available"}),
        Err(error) => json!({"status": "FAIL", "message": error.to_string()}),
    }
}

async fn launcher(command: LauncherCommand) -> Result<()> {
    let home = env::var_os("HOME").context("HOME is not set")?;
    let store = DesktopLauncherStore::new(PathBuf::from(home).join(".local/share/applications"));
    match command {
        LauncherCommand::Create { target } => {
            let node_id =
                NodeId::from_str(&target).context("launcher target must be a stable node ID")?;
            println!(
                "{}",
                store
                    .create(LauncherSpec {
                        node_id,
                        display_name: target,
                        aliases: Vec::new()
                    })
                    .await?
                    .display()
            );
        }
        LauncherCommand::List => {
            for path in store.list().await? {
                println!("{}", path.display());
            }
        }
        LauncherCommand::Remove { target } => {
            store.remove(NodeId::from_str(&target)?).await?;
        }
    }

    Ok(())
}

async fn setup() -> Result<()> {
    let directory = state_dir()?;

    std::fs::create_dir_all(directory.join("identity"))?;
    std::fs::create_dir_all(directory.join("access"))?;

    provision_sunshine_credentials().await?;

    doctor(false).await
}

async fn provision_sunshine_credentials() -> Result<()> {
    let store = sunshine_credential_store()?;
    let lookup = store.clone();

    if tokio::task::spawn_blocking(move || lookup.load())
        .await
        .context("query desktop Secret Service")??
        .is_some()
    {
        println!("Sunshine credentials already configured in the desktop Secret Service");
        return Ok(());
    }

    let username = env::var("OMDESKY_SUNSHINE_USERNAME").ok();
    let password = env::var("OMDESKY_SUNSHINE_PASSWORD").ok();
    let (username, password) = match (username, password) {
        (Some(username), Some(password)) => (username, password),
        _ => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                println!(
                    "Skipping Sunshine credential setup (no terminal). Set OMDESKY_SUNSHINE_USERNAME and OMDESKY_SUNSHINE_PASSWORD, or run `omdesky setup` interactively on the host."
                );
                return Ok(());
            }
            let username = prompt("Sunshine admin username: ")?;
            let password = rpassword::prompt_password("Sunshine admin password: ")?;
            (username, password)
        }
    };

    if username.is_empty() || password.is_empty() {
        println!("Skipping Sunshine credential setup (empty username or password)");
        return Ok(());
    }

    tokio::task::spawn_blocking(move || store.store(&username, &password))
        .await
        .context("update desktop Secret Service")?
        .context("store Sunshine credentials in the desktop Secret Service")?;
    println!("Stored Sunshine credentials in the desktop Secret Service");
    Ok(())
}

fn prompt(label: &str) -> Result<String> {
    use std::io::Write;

    print!("{label}");
    std::io::stdout().flush()?;

    let mut value = String::new();
    std::io::stdin().read_line(&mut value)?;

    Ok(value.trim().to_owned())
}

fn moonlight_adapter() -> Arc<MoonlightAdapter> {
    Arc::new(MoonlightAdapter::new(Arc::new(TokioCommandRunner)))
}

fn client_name() -> String {
    env::var("HOSTNAME").unwrap_or_else(|_| "omdesky".to_owned())
}

fn parse_workspace_target(value: &str) -> WorkspaceTarget {
    match value.parse() {
        Ok(id) => WorkspaceTarget::Id(id),
        Err(_) => WorkspaceTarget::Name(value.to_owned()),
    }
}

async fn remote_command(action: CommandAction) -> Result<()> {
    match action {
        CommandAction::SendShortcut {
            target,
            mods,
            key,
            window_class,
            window_address,
            port,
        } => {
            let endpoint = match (target.parse::<IpAddr>(), port) {
                (Ok(address), Some(port)) => AgentEndpoint { address, port },
                _ => resolve_endpoint(&target).await?,
            };

            let modifiers = parse_modifiers(&mods)?;
            let chord = KeyChord::new(modifiers, key).context("invalid key chord")?;
            let window = window_selector(window_class, window_address);

            let command = RemoteCommand::SendShortcut { chord, window };
            command.validate().context("invalid command")?;

            agent_client()
                .send_command(&endpoint, CommandRequest::new(command))
                .await?;
            println!("ok");

            Ok(())
        }
        CommandAction::CloseWindow {
            target,
            window_class,
            window_address,
            port,
        } => {
            let endpoint = match (target.parse::<IpAddr>(), port) {
                (Ok(address), Some(port)) => AgentEndpoint { address, port },
                _ => resolve_endpoint(&target).await?,
            };

            let window = window_selector(window_class, window_address);

            let command = RemoteCommand::CloseWindow { window };
            command.validate().context("invalid command")?;

            agent_client()
                .send_command(&endpoint, CommandRequest::new(command))
                .await?;
            println!("ok");

            Ok(())
        }
        CommandAction::SwitchDisplay {
            target,
            display,
            port,
        } => {
            let endpoint = match (target.parse::<IpAddr>(), port) {
                (Ok(address), Some(port)) => AgentEndpoint { address, port },
                _ => resolve_endpoint(&target).await?,
            };

            let command = RemoteCommand::SwitchStreamDisplay {
                display: DisplayId::new(display),
            };
            command.validate().context("invalid command")?;

            agent_client()
                .send_command(&endpoint, CommandRequest::new(command))
                .await?;
            println!("ok");

            Ok(())
        }
    }
}

fn window_selector(class: Option<String>, address: Option<String>) -> WindowSelector {
    match (class, address) {
        (_, Some(address)) => WindowSelector::Address(address),
        (Some(class), None) => WindowSelector::Class(class),
        (None, None) => WindowSelector::ActiveWindow,
    }
}

fn parse_capabilities(values: &[String]) -> Result<Vec<ControlCapability>> {
    if values.is_empty() {
        return Ok(ControlCapability::ALL.to_vec());
    }

    values
        .iter()
        .flat_map(|value| value.split(','))
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| {
            token.parse::<ControlCapability>().map_err(|_| {
                anyhow::anyhow!(
                    "unknown capability '{token}'; expected one of {}",
                    ControlCapability::ALL
                        .map(ControlCapability::as_str)
                        .join(", ")
                )
            })
        })
        .collect()
}

fn parse_modifiers(input: &str) -> Result<Vec<KeyModifier>> {
    input
        .split(['+', ' ', ','])
        .map(str::trim)
        .filter(|token| !token.is_empty())
        .map(|token| match token.to_ascii_lowercase().as_str() {
            "super" | "win" | "meta" | "mod" => Ok(KeyModifier::Super),
            "ctrl" | "control" => Ok(KeyModifier::Ctrl),
            "alt" => Ok(KeyModifier::Alt),
            "shift" => Ok(KeyModifier::Shift),
            other => Err(anyhow::anyhow!("unknown modifier: {other}")),
        })
        .collect()
}

async fn resolve_endpoint(target: &str) -> Result<AgentEndpoint> {
    let config = Config::load()?;
    let port = config.network.agent_port;

    if let Ok(address) = target.parse::<IpAddr>() {
        return Ok(AgentEndpoint { address, port });
    }

    let resolved = config
        .devices
        .get(target)
        .and_then(|device| device.alias.clone())
        .unwrap_or_else(|| target.to_owned());

    let peers = TailscaleAdapter::new(Arc::new(TokioCommandRunner))
        .peers()
        .await?;
    let address = peers
        .into_iter()
        .find(|peer| {
            peer.online
                && (peer.hostname.as_deref() == Some(resolved.as_str())
                    || peer.tailnet_node_id == resolved
                    || peer
                        .dns_name
                        .as_deref()
                        .is_some_and(|dns| dns.trim_end_matches('.').starts_with(&resolved)))
        })
        .and_then(|peer| {
            peer.ips
                .iter()
                .find(|address| address.is_ipv4())
                .or(peer.ips.first())
                .copied()
        })
        .with_context(|| format!("no online Tailnet peer matches '{target}'"))?;

    Ok(AgentEndpoint { address, port })
}

async fn resolve_tailnet_identity(target: &str) -> Result<String> {
    if let Ok(address) = target.parse::<IpAddr>() {
        return TailscaleAdapter::new(Arc::new(TokioCommandRunner))
            .identify_source(address)
            .await?
            .map(|identity| identity.tailnet_node_id)
            .context("target is not a visible Tailscale identity");
    }

    TailscaleAdapter::new(Arc::new(TokioCommandRunner))
        .peers()
        .await?
        .into_iter()
        .find(|peer| peer.hostname.as_deref() == Some(target) || peer.tailnet_node_id == target)
        .map(|peer| peer.tailnet_node_id)
        .with_context(|| format!("no Tailnet peer matches '{target}'"))
}

fn read_session() -> Option<SessionRecord> {
    SessionRecord::read(&runtime_session_path().ok()?).ok()
}

fn terminate_process(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("/usr/bin/kill")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}
