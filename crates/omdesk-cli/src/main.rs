use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand, ValueEnum};
use omdesk_application::{
    ports::{
        AccessStore, AgentClient, AgentEndpoint, AllowedController, LauncherSpec, LauncherStore,
        MeshNetwork, StreamHost,
    },
    services::{ConnectNode, ConnectRequest, DiscoverNodes, PairStream},
};
use omdesk_core::{
    CodecPreference, InputMode, KeyChord, KeyModifier, NodeId, RemoteCommand, StreamProfile,
    WindowSelector, WorkspaceTarget,
};
use omdesk_platform::{
    access::FileAccessStore,
    agent_client::HttpAgentClient,
    config::{
        Config, access_path, runtime_session_path, state_dir, sunshine_config_path,
        sunshine_credentials_path,
    },
    hyprland::HyprlandAdapter,
    input::HyprlandSessionKeybinds,
    launcher::DesktopLauncherStore,
    moonlight::MoonlightAdapter,
    omarchy::{OmarchyNotificationAdapter, detect_version},
    process::TokioCommandRunner,
    sunshine::SunshineAdapter,
    tailscale::TailscaleAdapter,
};
use omdesk_protocol::SunshinePairRequest;
use serde_json::json;
use std::{env, net::IpAddr, path::PathBuf, str::FromStr, sync::Arc};
use time::OffsetDateTime;
use tracing_subscriber::EnvFilter;

#[derive(Parser)]
#[command(name = "omdesk", version, about = "Omarchy DeskLink")]
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
    #[arg(long, value_enum, default_value_t = InputArg::Local)]
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

        #[arg(long)]
        window_class: Option<String>,

        #[arg(long)]
        port: Option<u16>,
    },
    CloseWindow {
        target: String,

        #[arg(long)]
        window_class: Option<String>,

        #[arg(long)]
        port: Option<u16>,
    },
}

#[derive(Subcommand)]
enum AccessCommand {
    List,
    Allow { peer: String },
    Revoke { peer: String },
}

#[derive(Subcommand)]
enum LauncherCommand {
    Create { target: String },
    List,
    Remove { target: String },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();

    match cli.command {
        None => omdesk_tui::run().await,
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
        omdesk_platform::sunshine::DEFAULT_API_BASE.to_owned(),
        Some(sunshine_credentials_path()?),
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
    let endpoint = resolve_endpoint(&args.target).await?;

    let notifications = Arc::new(OmarchyNotificationAdapter::default());
    let service = ConnectNode::new(
        agent_client(),
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

    let session_path = runtime_session_path().ok();
    write_session(session_path.as_deref(), &args.target, input_mode);

    let controller_endpoint = if input_mode == InputMode::Remote {
        local_agent_endpoint(config.network.agent_port)
            .await
            .map_err(|error| {
                anyhow::anyhow!("could not resolve this controller's Tailscale endpoint: {error}")
            })?
    } else {
        endpoint.clone()
    };

    let exit = service
        .execute(ConnectRequest {
            endpoint: endpoint.clone(),
            controller_endpoint,
            profile: StreamProfile {
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
                bitrate_kbps: args
                    .bitrate
                    .or_else(|| {
                        (config.stream.bitrate_mbps > 0).then_some(config.stream.bitrate_mbps)
                    })
                    .map(|mbps| mbps * 1000),
            },
            fullscreen: args.fullscreen || !args.windowed,
            input_mode,
            focus_workspace,
            focus_window: args.window.or(args.app_id),
            auto_pair: true,
        })
        .await;

    if let Some(path) = &session_path {
        let _ = std::fs::remove_file(path);
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
            let mode =
                read_session().and_then(|value| value["input_mode"].as_str().map(str::to_owned));
            println!("input mode: {}", mode.as_deref().unwrap_or("local"));
            Ok(())
        }
        InputCommand::Local | InputCommand::Remote | InputCommand::Toggle => anyhow::bail!(
            "live input switching is not supported by the installed Moonlight; set --input at connect time"
        ),
    }
}

async fn session(json: bool) -> Result<()> {
    match read_session() {
        Some(value) => {
            if json {
                println!("{value}");
            } else {
                println!(
                    "node {}  ·  pid {}  ·  input {}",
                    value["remote_node"].as_str().unwrap_or("—"),
                    value["moonlight_pid"].as_i64().unwrap_or(0),
                    value["input_mode"].as_str().unwrap_or("local")
                );
            }
            Ok(())
        }
        None => {
            if json {
                println!("null");
                Ok(())
            } else {
                anyhow::bail!("no active DeskLink session")
            }
        }
    }
}

async fn disconnect() -> Result<()> {
    let value = read_session().context("no active DeskLink session to disconnect")?;

    if let Some(pid) = value["moonlight_pid"].as_i64().filter(|pid| *pid > 0) {
        terminate_process(pid as u32);
        println!("Signalled Moonlight process {pid} to stop");
    } else {
        println!("Session has no supervised Moonlight process");
    }

    if let Ok(path) = runtime_session_path() {
        let _ = std::fs::remove_file(path);
    }
    Ok(())
}

async fn access(command: AccessCommand) -> Result<()> {
    let store = FileAccessStore::new(access_path()?);
    match command {
        AccessCommand::List => {
            for entry in store.list().await? {
                println!(
                    "{}\t{}",
                    entry.tailnet_node_id,
                    entry.label.as_deref().unwrap_or("—")
                );
            }
        }
        AccessCommand::Allow { peer } => {
            let identity = resolve_tailnet_identity(&peer).await?;
            store
                .allow(AllowedController {
                    tailnet_node_id: identity,
                    label: Some(peer.clone()),
                    added_at: OffsetDateTime::now_utc(),
                })
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
        omdesk_platform::sunshine::DEFAULT_API_BASE.to_owned(),
        Some(sunshine_credentials_path()?),
    )
    .readiness()
    .await;

    let credentials_path = sunshine_credentials_path()?;
    let sunshine_pairing = if credentials_path.exists() {
        json!({"status": "PASS", "message": "Sunshine admin credentials configured on this host"})
    } else {
        json!({
            "status": "WARN",
            "message": "Sunshine admin credentials missing; run `omdesk setup` on this host to enable pairing"
        })
    };

    let report = json!({
        "omarchy": check(&omarchy),
        "tailscale": check(&tailscale),
        "hyprland": check(&hyprland),
        "sunshine": check(&sunshine),
        "sunshine_pairing": sunshine_pairing,
        "syncthing": {"status": "SKIP", "message": "optional integration disabled"}
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

    provision_sunshine_credentials()?;

    doctor(false).await
}

fn provision_sunshine_credentials() -> Result<()> {
    let path = sunshine_credentials_path()?;
    if path.exists() {
        println!(
            "Sunshine credentials already configured at {}",
            path.display()
        );
        return Ok(());
    }

    let username = env::var("OMDESK_SUNSHINE_USERNAME").ok();
    let password = env::var("OMDESK_SUNSHINE_PASSWORD").ok();
    let (username, password) = match (username, password) {
        (Some(username), Some(password)) => (username, password),
        _ => {
            if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
                println!(
                    "Skipping Sunshine credential setup (no terminal). Set OMDESK_SUNSHINE_USERNAME and OMDESK_SUNSHINE_PASSWORD, or run `omdesk setup` interactively on the host."
                );
                return Ok(());
            }
            let username = prompt("Sunshine admin username: ")?;
            let password = prompt("Sunshine admin password: ")?;
            (username, password)
        }
    };

    if username.is_empty() || password.is_empty() {
        println!("Skipping Sunshine credential setup (empty username or password)");
        return Ok(());
    }

    omdesk_platform::sunshine::store_credentials(&path, &username, &password)
        .context("write Sunshine credentials")?;
    println!("Stored Sunshine credentials at {}", path.display());
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
    env::var("HOSTNAME").unwrap_or_else(|_| "desklink".to_owned())
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
            port,
        } => {
            let endpoint = match (target.parse::<IpAddr>(), port) {
                (Ok(address), Some(port)) => AgentEndpoint { address, port },
                _ => resolve_endpoint(&target).await?,
            };

            let modifiers = parse_modifiers(&mods)?;
            let chord = KeyChord::new(modifiers, key).context("invalid key chord")?;
            let window = window_class.map_or(WindowSelector::ActiveWindow, WindowSelector::Class);

            let command = RemoteCommand::SendShortcut { chord, window };
            command.validate().context("invalid command")?;

            agent_client().send_command(&endpoint, command).await?;
            println!("ok");

            Ok(())
        }
        CommandAction::CloseWindow {
            target,
            window_class,
            port,
        } => {
            let endpoint = match (target.parse::<IpAddr>(), port) {
                (Ok(address), Some(port)) => AgentEndpoint { address, port },
                _ => resolve_endpoint(&target).await?,
            };

            let window = window_class.map_or(WindowSelector::ActiveWindow, WindowSelector::Class);

            let command = RemoteCommand::CloseWindow { window };
            command.validate().context("invalid command")?;

            agent_client().send_command(&endpoint, command).await?;
            println!("ok");

            Ok(())
        }
    }
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

fn write_session(path: Option<&std::path::Path>, node: &str, input_mode: InputMode) {
    let Some(path) = path else {
        return;
    };
    let record = json!({
        "session_id": uuid_like(),
        "remote_node": node,
        "moonlight_pid": std::process::id(),
        "input_mode": match input_mode {
            InputMode::Local => "local",
            InputMode::Remote => "remote",
        },
        "started_at": OffsetDateTime::now_utc().unix_timestamp(),
    });
    let _ = omdesk_platform::state::atomic_write_json(path, &record, false);
}

fn read_session() -> Option<serde_json::Value> {
    let path = runtime_session_path().ok()?;
    let content = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

fn uuid_like() -> String {
    uuid::Uuid::new_v4().to_string()
}

fn terminate_process(pid: u32) {
    #[cfg(unix)]
    {
        let _ = std::process::Command::new("kill")
            .arg(pid.to_string())
            .status();
    }
    #[cfg(not(unix))]
    {
        let _ = pid;
    }
}
