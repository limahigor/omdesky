#![forbid(unsafe_code)]

use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEventKind,
};
use omdesky_application::{
    access::CallbackAccess,
    discovery::{DiscoverNodes, DiscoveredNode, blocker_message},
    ports::{
        AccessStore, AgentClient, AgentEndpoint, AllowedController, CommandExecutor, MeshNetwork,
        PortError, StreamWindowLocator,
    },
    readiness::ensure_controller_ready,
    services::{ConnectNode, ConnectRequest, MOONLIGHT_WINDOW_CLASS},
};
use omdesky_core::{
    BlockerCode, CodecPreference, ConnectionKind, ControlCapability, DisplayMode, InputMode,
    NodeStatus, RemoteCommand, StreamProfile, WindowSelector, bitrate_kbps_from_mbps,
};
use omdesky_platform::{
    access::FileAccessStore,
    agent_client::HttpAgentClient,
    config::{Config, access_path, state_dir},
    hyprland::HyprlandAdapter,
    input::{HyprlandCommandExecutor, HyprlandSessionKeybinds},
    moonlight::MoonlightAdapter,
    omarchy::OmarchyNotificationAdapter,
    process::TokioCommandRunner,
    sunshine::SunshineCredentialStore,
    tailscale::TailscaleAdapter,
    theme::{OmarchyTheme, Rgb, ThemeWatcher},
};
use ratatui::{
    DefaultTerminal, Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, BorderType, Borders, Clear, List, ListItem, ListState, Padding, Paragraph},
};
use std::{
    env,
    path::PathBuf,
    sync::Arc,
    time::{Duration, Instant},
};
use time::OffsetDateTime;
use tokio::sync::mpsc;

pub async fn run() -> anyhow::Result<()> {
    let services = Services::new()?;
    let theme = ThemeWatcher::new(theme_path());

    let mut terminal = ratatui::init();
    let _ = crossterm::execute!(std::io::stdout(), EnableMouseCapture);

    let mut state = AppState::new(theme);
    let result = run_loop(&mut terminal, services, &mut state).await;

    let _ = crossterm::execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();

    result
}

struct Services {
    discovery: Arc<DiscoverNodes>,
    agent: Arc<HttpAgentClient>,
    stream: Arc<MoonlightAdapter>,
    notifications: Arc<OmarchyNotificationAdapter>,
    desktop: Arc<HyprlandCommandExecutor>,
    windows: Arc<HyprlandAdapter>,
    access: Arc<FileAccessStore>,
    callback_access: CallbackAccess,
    sunshine_credentials: SunshineCredentialStore,
    agent_port: u16,
    client_name: String,
}

impl Services {
    fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        let runner = Arc::new(TokioCommandRunner);
        let mesh = Arc::new(TailscaleAdapter::new(runner.clone()));
        let agent = Arc::new(HttpAgentClient::new());
        let access = Arc::new(FileAccessStore::new(access_path()?));

        let _ = state_dir();

        Ok(Self {
            discovery: Arc::new(DiscoverNodes::new(
                mesh.clone(),
                agent.clone(),
                access.clone(),
                config.network.agent_port,
            )),
            callback_access: CallbackAccess::new(mesh, access.clone()),
            agent,
            stream: Arc::new(MoonlightAdapter::new(runner.clone())),
            notifications: Arc::new(OmarchyNotificationAdapter::default()),
            desktop: Arc::new(HyprlandCommandExecutor::new(runner.clone())),
            windows: Arc::new(HyprlandAdapter::new(runner)),
            access,
            sunshine_credentials: SunshineCredentialStore::default(),
            agent_port: config.network.agent_port,
            client_name: env::var("HOSTNAME").unwrap_or_else(|_| "omdesky".to_owned()),
        })
    }

    fn connect_service(&self) -> ConnectNode {
        ConnectNode::new(
            self.agent.clone(),
            self.callback_access.clone(),
            self.stream.clone(),
            Arc::new(HyprlandSessionKeybinds::new(Arc::new(TokioCommandRunner))),
            self.notifications.clone(),
            self.client_name.clone(),
        )
        .with_window_locator(self.windows.clone())
    }
}

fn theme_path() -> PathBuf {
    env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(env::var_os("HOME").unwrap_or_default()).join(".local/state")
        })
        .join("omarchy/current/theme/colors.toml")
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Activity {
    Idle,
    Connecting,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SessionAction {
    Start,
    Focus,
    Switch,
}

fn session_action(active_node_id: Option<&str>, selected_node_id: &str) -> SessionAction {
    match active_node_id {
        Some(active) if active == selected_node_id => SessionAction::Focus,
        Some(_) => SessionAction::Switch,
        None => SessionAction::Start,
    }
}

type DiscoveryResult = Result<Vec<DiscoveredNode>, String>;

#[derive(Clone, Debug, Eq, PartialEq)]
struct AccessRow {
    tailnet_node_id: String,
    label: String,
}

enum Overlay {
    Menu { selected: StreamSetting },
    CredentialsUser { user: String },
    CredentialsPass { user: String, pass: String },
    Access,
}

const NOTICE_LIFETIME: Duration = Duration::from_secs(5);
const ERROR_LIFETIME: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    Notice,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamSetting {
    Display,
    Resolution,
    Fps,
    Bitrate,
    Codec,
    Audio,
}

impl StreamSetting {
    const ALL: [Self; 6] = [
        Self::Display,
        Self::Resolution,
        Self::Fps,
        Self::Bitrate,
        Self::Codec,
        Self::Audio,
    ];

    fn next(self) -> Self {
        Self::ALL[(self.index() + 1) % Self::ALL.len()]
    }

    fn previous(self) -> Self {
        Self::ALL[(self.index() + Self::ALL.len() - 1) % Self::ALL.len()]
    }

    fn index(self) -> usize {
        Self::ALL
            .iter()
            .position(|setting| *setting == self)
            .unwrap_or(0)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum LocalReadiness {
    Checking,
    Ready,
    Blocked(String),
}

enum AsyncMessage {
    Discovery(DiscoveryResult),
    LocalReadiness(Result<(), String>),
    SessionStarted {
        generation: u64,
        node_id: String,
        node_name: String,
        moonlight_pid: Option<u32>,
    },
    SessionEnded {
        generation: u64,
        result: Result<String, String>,
        local_failure: bool,
    },
    Focus(Result<String, String>),
    Access(Result<Vec<AccessRow>, String>),
    CredentialStatus(Result<bool, String>),
    CredentialStored(Result<(), String>),
}

struct AppState {
    nodes: Vec<DiscoveredNode>,
    list: ListState,
    theme: ThemeWatcher,
    loading: bool,
    activity: Activity,
    active_node_id: Option<String>,
    active_moonlight_pid: Option<u32>,
    pending_node: Option<DiscoveredNode>,
    session_generation: u64,
    notice: Option<String>,
    error: Option<String>,
    overlay: Option<Overlay>,
    sunshine_configured: bool,
    local_readiness: LocalReadiness,
    access_entries: Vec<AccessRow>,
    stream_profile: StreamProfile,
    display_mode: DisplayMode,
    detail_expanded: bool,
    detail_overflow: bool,
    detail_rect: Option<Rect>,
    message_shown: Option<ShownMessage>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct ShownMessage {
    kind: MessageKind,
    text: String,
    since: Instant,
}

impl AppState {
    fn new(theme: ThemeWatcher) -> Self {
        Self {
            nodes: Vec::new(),
            list: ListState::default(),
            theme,
            loading: false,
            activity: Activity::Idle,
            active_node_id: None,
            active_moonlight_pid: None,
            pending_node: None,
            session_generation: 0,
            notice: None,
            error: None,
            overlay: None,
            sunshine_configured: false,
            local_readiness: LocalReadiness::Checking,
            access_entries: Vec::new(),
            stream_profile: StreamProfile::default(),
            display_mode: DisplayMode::default(),
            detail_expanded: false,
            detail_overflow: false,
            detail_rect: None,
            message_shown: None,
        }
    }

    fn expire_message(&mut self, now: Instant) {
        let Some((kind, text)) = self.message().map(|(kind, text)| (kind, text.to_owned())) else {
            self.message_shown = None;
            return;
        };

        let operation_pending = self.activity != Activity::Idle || self.pending_node.is_some();

        let unchanged = self
            .message_shown
            .as_ref()
            .is_some_and(|shown| shown.kind == kind && shown.text == text);

        if operation_pending || !unchanged {
            self.message_shown = Some(ShownMessage {
                kind,
                text,
                since: now,
            });
            return;
        }

        let lifetime = match kind {
            MessageKind::Notice => NOTICE_LIFETIME,
            MessageKind::Error => ERROR_LIFETIME,
        };

        let expired = self
            .message_shown
            .as_ref()
            .is_some_and(|shown| now.duration_since(shown.since) >= lifetime);

        if expired {
            self.error = None;
            self.notice = None;
            self.message_shown = None;
        }
    }

    fn message(&self) -> Option<(MessageKind, &str)> {
        if let Some(error) = &self.error {
            Some((MessageKind::Error, error.as_str()))
        } else {
            self.notice
                .as_deref()
                .map(|notice| (MessageKind::Notice, notice))
        }
    }

    fn selected(&self) -> Option<&DiscoveredNode> {
        self.list.selected().and_then(|index| self.nodes.get(index))
    }

    fn selected_remote(&self) -> Option<DiscoveredNode> {
        self.selected()
            .filter(|node| node.is_connectable())
            .cloned()
    }

    fn unavailable_reason(&self) -> Option<String> {
        if let LocalReadiness::Blocked(error) = &self.local_readiness {
            return Some(error.clone());
        }

        if self.local_readiness == LocalReadiness::Checking {
            return Some("Checking local connection requirements. Please wait.".to_owned());
        }

        let node = self.selected()?;

        if node.is_local {
            return Some("This device cannot connect to itself".to_owned());
        }

        if let Some(blocker) = node.blockers.first() {
            return Some(blocker_message(&node.name, blocker));
        }

        match node.status {
            NodeStatus::Ready => None,
            NodeStatus::Blocked | NodeStatus::Offline | NodeStatus::Unavailable => {
                Some("This device is not ready".to_owned())
            }
        }
    }

    fn select_first(&mut self) {
        if self.nodes.is_empty() {
            self.list.select(None);
        } else {
            self.list.select(Some(0));
        }
    }

    fn select_next(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        let next = self
            .list
            .selected()
            .map_or(0, |index| (index + 1) % self.nodes.len());
        self.list.select(Some(next));
        self.detail_expanded = false;
    }

    fn select_previous(&mut self) {
        if self.nodes.is_empty() {
            return;
        }

        let previous = self
            .list
            .selected()
            .map_or(0, |index| (index + self.nodes.len() - 1) % self.nodes.len());
        self.list.select(Some(previous));
        self.detail_expanded = false;
    }
}

fn start_refresh(discovery: Arc<DiscoverNodes>, sender: mpsc::Sender<AsyncMessage>) {
    tokio::spawn(async move {
        let result = discovery
            .execute(false)
            .await
            .map_err(|error| user_error("Devices could not be refreshed.", &error));
        let _ = sender.send(AsyncMessage::Discovery(result)).await;
    });
}

fn start_local_readiness(
    agent: Arc<HttpAgentClient>,
    credentials: SunshineCredentialStore,
    agent_port: u16,
    sender: mpsc::Sender<AsyncMessage>,
) {
    tokio::spawn(async move {
        let endpoint = local_agent_endpoint(agent_port).await;
        let local_agent = match endpoint {
            Ok(endpoint) => agent.health(&endpoint).await,
            Err(error) => Err(PortError::new(
                "LOCAL_AGENT_UNAVAILABLE",
                error.to_string(),
                true,
            )),
        };
        let sunshine_configured = credentials.configured().await.unwrap_or(false);
        let result = ensure_controller_ready(local_agent, sunshine_configured)
            .map_err(|error| error.user_message().to_owned());

        let _ = sender.send(AsyncMessage::LocalReadiness(result)).await;
    });
}

fn start_connect(
    services: &Services,
    node: DiscoveredNode,
    generation: u64,
    sender: mpsc::Sender<AsyncMessage>,
) {
    let service = services.connect_service();
    let endpoint = AgentEndpoint {
        address: node.address,
        port: services.agent_port,
    };
    let agent = services.agent.clone();
    let agent_port = services.agent_port;
    let credentials = services.sunshine_credentials.clone();
    let profile = Config::load()
        .map(|config| stream_profile(&config))
        .unwrap_or_else(|_| StreamProfile::default());
    let input_mode = InputMode::Remote;

    tokio::spawn(async move {
        let controller_endpoint = match local_agent_endpoint(agent_port).await {
            Ok(endpoint) => endpoint,
            Err(error) => {
                let result = Err(format!(
                    "The connection could not be completed. {}",
                    PortError::new("LOCAL_AGENT_UNAVAILABLE", error.to_string(), true)
                        .user_message()
                ));
                let _ = sender
                    .send(AsyncMessage::SessionEnded {
                        generation,
                        result,
                        local_failure: true,
                    })
                    .await;
                return;
            }
        };
        let local_agent = agent.health(&controller_endpoint).await;
        let sunshine_configured = credentials.configured().await.unwrap_or(false);
        if let Err(error) = ensure_controller_ready(local_agent, sunshine_configured) {
            let result = Err(user_error("The connection could not be completed.", &error));
            let _ = sender
                .send(AsyncMessage::SessionEnded {
                    generation,
                    result,
                    local_failure: true,
                })
                .await;
            return;
        }

        let node_id = node.tailnet_node_id.clone();
        let node_name = node.name.clone();
        let started_sender = sender.clone();
        let result = service
            .execute_with_started(
                ConnectRequest {
                    endpoint,
                    controller_endpoint,
                    profile,
                    fullscreen: true,
                    input_mode,
                    focus_workspace: None,
                    focus_window: None,
                    auto_pair: true,
                },
                |moonlight_pid| async move {
                    let _ = started_sender
                        .send(AsyncMessage::SessionStarted {
                            generation,
                            node_id,
                            node_name,
                            moonlight_pid,
                        })
                        .await;
                },
            )
            .await;
        let local_failure = result.as_ref().is_err_and(|error| {
            matches!(
                error.code,
                "LOCAL_AGENT_UNAVAILABLE" | "LOCAL_SUNSHINE_UNCONFIGURED"
            )
        });
        let result = result
            .map(|exit| stream_exit_message(&node.name, exit))
            .map_err(|error| user_error("The connection could not be completed.", &error));
        let _ = sender
            .send(AsyncMessage::SessionEnded {
                generation,
                result,
                local_failure,
            })
            .await;
    });
}

fn start_focus(
    desktop: Arc<HyprlandCommandExecutor>,
    windows: Arc<HyprlandAdapter>,
    moonlight_pid: Option<u32>,
    sender: mpsc::Sender<AsyncMessage>,
) {
    tokio::spawn(async move {
        let window = stream_window(windows.as_ref(), moonlight_pid).await;

        let result = desktop
            .focus_stream(&window)
            .await
            .map(|_| "Focused the active stream.".to_owned())
            .map_err(|error| user_error("The active stream could not be focused.", &error));

        let _ = sender.send(AsyncMessage::Focus(result)).await;
    });
}

async fn stream_window(windows: &HyprlandAdapter, moonlight_pid: Option<u32>) -> WindowSelector {
    let resolved = match moonlight_pid {
        Some(pid) => windows.window_for_process(pid).await.ok().flatten(),
        None => None,
    };

    resolved.unwrap_or_else(|| WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned()))
}

async fn local_agent_endpoint(port: u16) -> anyhow::Result<AgentEndpoint> {
    let local = TailscaleAdapter::new(Arc::new(TokioCommandRunner))
        .local_node()
        .await?;
    let address = local
        .addresses
        .iter()
        .find(|address| address.is_ipv4())
        .or(local.addresses.first())
        .copied()
        .ok_or_else(|| anyhow::anyhow!("Tailscale has no local address"))?;

    Ok(AgentEndpoint { address, port })
}

fn start_access_list(access: Arc<FileAccessStore>, sender: mpsc::Sender<AsyncMessage>) {
    tokio::spawn(async move {
        let result = access
            .list()
            .await
            .map(|entries| {
                entries
                    .into_iter()
                    .map(|entry| AccessRow {
                        label: entry.label.unwrap_or_else(|| entry.tailnet_node_id.clone()),
                        tailnet_node_id: entry.tailnet_node_id,
                    })
                    .collect()
            })
            .map_err(|error| user_error("The allowed devices list could not be loaded.", &error));
        let _ = sender.send(AsyncMessage::Access(result)).await;
    });
}

fn start_access_allow(
    access: Arc<FileAccessStore>,
    controller: AllowedController,
    sender: mpsc::Sender<AsyncMessage>,
) {
    tokio::spawn(async move {
        if let Err(error) = access.allow(controller).await {
            let _ = sender
                .send(AsyncMessage::Access(Err(user_error(
                    "This device could not be allowed.",
                    &error,
                ))))
                .await;
            return;
        }

        start_access_list(access, sender);
    });
}

fn start_access_revoke(
    access: Arc<FileAccessStore>,
    tailnet_node_id: String,
    sender: mpsc::Sender<AsyncMessage>,
) {
    tokio::spawn(async move {
        if let Err(error) = access.revoke(&tailnet_node_id).await {
            let _ = sender
                .send(AsyncMessage::Access(Err(user_error(
                    "Access for this device could not be removed.",
                    &error,
                ))))
                .await;
            return;
        }

        start_access_list(access, sender);
    });
}

fn start_credential_status(
    credentials: SunshineCredentialStore,
    sender: mpsc::Sender<AsyncMessage>,
) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || credentials.load())
            .await
            .map_err(|_| "The desktop Secret Service could not be queried".to_owned())
            .and_then(|result| {
                result
                    .map(|credentials| credentials.is_some())
                    .map_err(|error| error.to_string())
            });

        let _ = sender.send(AsyncMessage::CredentialStatus(result)).await;
    });
}

fn start_credential_store(
    credentials: SunshineCredentialStore,
    username: String,
    password: String,
    sender: mpsc::Sender<AsyncMessage>,
) {
    tokio::spawn(async move {
        let result = tokio::task::spawn_blocking(move || credentials.store(&username, &password))
            .await
            .map_err(|_| "The desktop Secret Service could not be updated".to_owned())
            .and_then(|result| result.map_err(|error| error.to_string()));

        let _ = sender.send(AsyncMessage::CredentialStored(result)).await;
    });
}

fn user_error(context: &str, error: &PortError) -> String {
    tracing::debug!(
        code = error.code,
        detail = %error.message,
        retryable = error.retryable,
        user_context = context,
        "tui.operation_failed"
    );
    format!("{context} {}", error.user_message())
}

fn stream_exit_message(node_name: &str, exit: i32) -> String {
    if exit == 0 {
        format!("Disconnected from {node_name}.")
    } else {
        tracing::debug!(node = node_name, exit, "stream.unexpected_exit");
        format!("The stream from {node_name} ended unexpectedly. You can try reconnecting.")
    }
}

fn requires_terminal_reset(message: &AsyncMessage) -> bool {
    matches!(
        message,
        AsyncMessage::SessionStarted { .. } | AsyncMessage::SessionEnded { .. }
    )
}

fn requires_local_readiness_refresh(message: &AsyncMessage) -> bool {
    matches!(message, AsyncMessage::CredentialStored(Ok(())))
}

fn apply_message(state: &mut AppState, message: AsyncMessage) {
    match message {
        AsyncMessage::Discovery(result) => apply_refresh(state, result),
        AsyncMessage::LocalReadiness(result) => match result {
            Ok(()) => {
                state.local_readiness = LocalReadiness::Ready;
                state.error = None;
            }
            Err(error) => {
                state.local_readiness = LocalReadiness::Blocked(error.clone());
                state.error = Some(error);
                state.notice = None;
            }
        },
        AsyncMessage::SessionStarted {
            generation,
            node_id,
            node_name,
            moonlight_pid,
        } if generation == state.session_generation => {
            state.activity = Activity::Idle;
            state.active_node_id = Some(node_id);
            state.active_moonlight_pid = moonlight_pid;
            state.notice = Some(format!("Connected to {node_name}."));
            state.error = None;
        }
        AsyncMessage::SessionStarted { .. } => {}
        AsyncMessage::SessionEnded {
            generation,
            result,
            local_failure,
        } if generation == state.session_generation => {
            state.activity = Activity::Idle;
            state.active_node_id = None;
            state.active_moonlight_pid = None;
            state.detail_expanded = false;

            if local_failure {
                state.local_readiness = LocalReadiness::Blocked(
                    result
                        .as_ref()
                        .err()
                        .cloned()
                        .unwrap_or_else(|| "The local controller is unavailable.".to_owned()),
                );
            }

            match result {
                Ok(notice) => {
                    state.notice = Some(notice);
                    state.error = None;
                }
                Err(error) => {
                    state.error = Some(error);
                    state.notice = None;
                }
            }
        }
        AsyncMessage::SessionEnded { .. } => {}
        AsyncMessage::Focus(result) => match result {
            Ok(notice) => {
                state.notice = Some(notice);
                state.error = None;
            }
            Err(error) => {
                state.error = Some(error);
                state.notice = None;
            }
        },
        AsyncMessage::Access(result) => match result {
            Ok(entries) => state.access_entries = entries,
            Err(error) => {
                state.detail_expanded = false;
                state.error = Some(error);
            }
        },
        AsyncMessage::CredentialStatus(result) => match result {
            Ok(configured) => state.sunshine_configured = configured,
            Err(error) => {
                state.local_readiness = LocalReadiness::Blocked(error.clone());
                state.error = Some(error);
            }
        },
        AsyncMessage::CredentialStored(result) => match result {
            Ok(()) => {
                state.sunshine_configured = true;
                state.local_readiness = LocalReadiness::Checking;
                state.notice = Some("Sunshine credentials saved".to_owned());
                state.error = None;
            }
            Err(error) => {
                state.error = Some(format!("Could not save credentials: {error}"));
                state.notice = None;
            }
        },
    }
}

fn apply_refresh(state: &mut AppState, result: DiscoveryResult) {
    state.loading = false;
    state.detail_expanded = false;

    match result {
        Ok(nodes) => {
            state.nodes = nodes;
            if state.local_readiness == LocalReadiness::Ready {
                state.error = None;
            }
            state.select_first();
        }
        Err(error) => {
            state.nodes.clear();
            state.error = Some(error);
            state.list.select(None);
        }
    }
}

async fn run_loop(
    terminal: &mut DefaultTerminal,
    services: Services,
    state: &mut AppState,
) -> anyhow::Result<()> {
    let (sender, mut receiver) = mpsc::channel(16);
    state.loading = true;
    start_credential_status(services.sunshine_credentials.clone(), sender.clone());
    start_local_readiness(
        services.agent.clone(),
        services.sunshine_credentials.clone(),
        services.agent_port,
        sender.clone(),
    );

    let loaded = Config::load().ok();
    state.stream_profile = loaded.as_ref().map(stream_profile).unwrap_or_default();
    state.display_mode = loaded.map(|config| config.display.mode).unwrap_or_default();
    start_refresh(services.discovery.clone(), sender.clone());

    loop {
        state.theme.refresh();

        while let Ok(message) = receiver.try_recv() {
            let reset_terminal = requires_terminal_reset(&message);
            let refresh_readiness = requires_local_readiness_refresh(&message);
            apply_message(state, message);
            if refresh_readiness {
                start_local_readiness(
                    services.agent.clone(),
                    services.sunshine_credentials.clone(),
                    services.agent_port,
                    sender.clone(),
                );
            }
            if reset_terminal {
                terminal.clear()?;
                terminal.autoresize()?;
            }
        }

        if state.activity == Activity::Idle
            && state.active_node_id.is_none()
            && let Some(node) = state.pending_node.take()
        {
            start_preflight(state, &services, node, sender.clone());
        }

        state.expire_message(Instant::now());

        terminal.draw(|frame| draw(frame, state))?;

        if !event::poll(Duration::from_millis(80))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_key(state, &services, &sender, key).await {
                    return Ok(());
                }
            }
            Event::Mouse(mouse) if mouse.kind == MouseEventKind::Down(MouseButton::Left) => {
                if state.detail_overflow
                    && let Some(rect) = state.detail_rect
                    && point_in_rect(mouse.column, mouse.row, rect)
                {
                    state.detail_expanded = !state.detail_expanded;
                }
            }
            _ => {}
        }
    }
}

fn point_in_rect(column: u16, row: u16, rect: Rect) -> bool {
    column >= rect.x && column < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

fn start_preflight(
    state: &mut AppState,
    services: &Services,
    node: DiscoveredNode,
    sender: mpsc::Sender<AsyncMessage>,
) {
    state.session_generation = state.session_generation.wrapping_add(1);
    state.activity = Activity::Connecting;
    state.active_node_id = None;
    state.error = None;
    state.notice = Some(format!("Checking connection to {}…", node.name));
    start_connect(services, node, state.session_generation, sender);
}

async fn handle_key(
    state: &mut AppState,
    services: &Services,
    sender: &mpsc::Sender<AsyncMessage>,
    key: KeyEvent,
) -> bool {
    if state.overlay.is_some() {
        handle_overlay_key(state, services, sender, key);
        return false;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Down | KeyCode::Char('j') => state.select_next(),
        KeyCode::Up | KeyCode::Char('k') => state.select_previous(),
        KeyCode::Char('r') if !state.loading => {
            state.loading = true;
            state.local_readiness = LocalReadiness::Checking;
            state.error = None;
            state.notice = None;
            start_refresh(services.discovery.clone(), sender.clone());
            start_local_readiness(
                services.agent.clone(),
                services.sunshine_credentials.clone(),
                services.agent_port,
                sender.clone(),
            );
        }
        KeyCode::Char('s') => {
            start_credential_status(services.sunshine_credentials.clone(), sender.clone());

            let loaded = Config::load().ok();
            state.stream_profile = loaded.as_ref().map(stream_profile).unwrap_or_default();
            state.display_mode = loaded.map(|config| config.display.mode).unwrap_or_default();
            state.overlay = Some(Overlay::Menu {
                selected: StreamSetting::Display,
            });
        }
        KeyCode::Enter => {
            if let Some(reason) = state.unavailable_reason() {
                state.error = Some(format!("ATTENTION: {reason}"));
                state.notice = None;
            } else if let Some(node) = state.selected_remote() {
                match session_action(state.active_node_id.as_deref(), &node.tailnet_node_id) {
                    SessionAction::Focus => {
                        state.notice = Some(format!("Focusing {}…", node.name));
                        state.error = None;
                        start_focus(
                            services.desktop.clone(),
                            services.windows.clone(),
                            state.active_moonlight_pid,
                            sender.clone(),
                        );
                    }
                    SessionAction::Switch => {
                        state.pending_node = Some(node.clone());
                        state.notice = Some(format!("Switching to {}…", node.name));
                        state.error = None;

                        let window =
                            stream_window(services.windows.as_ref(), state.active_moonlight_pid)
                                .await;

                        if let Err(error) = services
                            .desktop
                            .execute(RemoteCommand::CloseWindow { window })
                            .await
                        {
                            state.pending_node = None;
                            state.error = Some(user_error(
                                "The current stream could not be closed.",
                                &error,
                            ));
                            state.notice = None;
                        }
                    }
                    SessionAction::Start => {
                        if state.activity == Activity::Idle {
                            start_preflight(state, services, node, sender.clone());
                        }
                    }
                }
            }
        }
        _ => {}
    }

    false
}

fn handle_overlay_key(
    state: &mut AppState,
    services: &Services,
    sender: &mpsc::Sender<AsyncMessage>,
    key: KeyEvent,
) {
    match state.overlay.as_mut() {
        Some(Overlay::Menu { selected }) => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => state.overlay = None,
            KeyCode::Down | KeyCode::Char('j') => *selected = selected.next(),
            KeyCode::Up | KeyCode::Char('k') => *selected = selected.previous(),
            KeyCode::Left | KeyCode::Char('h') => {
                if *selected == StreamSetting::Display {
                    change_display_mode(state, -1);
                } else {
                    change_stream_setting(&mut state.stream_profile, *selected, -1);
                    save_stream_profile(state);
                }
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                if *selected == StreamSetting::Display {
                    change_display_mode(state, 1);
                } else {
                    change_stream_setting(&mut state.stream_profile, *selected, 1);
                    save_stream_profile(state);
                }
            }
            KeyCode::Char('c') => {
                state.overlay = Some(Overlay::CredentialsUser {
                    user: String::new(),
                });
            }
            KeyCode::Char('a') => {
                state.overlay = Some(Overlay::Access);
                start_access_list(services.access.clone(), sender.clone());
            }
            _ => {}
        },
        Some(Overlay::CredentialsUser { user }) => match key.code {
            KeyCode::Esc => {
                state.overlay = Some(Overlay::Menu {
                    selected: StreamSetting::Resolution,
                });
            }
            KeyCode::Backspace => {
                user.pop();
            }
            KeyCode::Char(character) if !character.is_control() => user.push(character),
            KeyCode::Enter if !user.is_empty() => {
                let user = user.clone();
                state.overlay = Some(Overlay::CredentialsPass {
                    user,
                    pass: String::new(),
                });
            }
            _ => {}
        },
        Some(Overlay::CredentialsPass { user, pass }) => match key.code {
            KeyCode::Esc => {
                state.overlay = Some(Overlay::Menu {
                    selected: StreamSetting::Resolution,
                });
            }
            KeyCode::Backspace => {
                pass.pop();
            }
            KeyCode::Char(character) if !character.is_control() => pass.push(character),
            KeyCode::Enter if !pass.is_empty() => {
                let username = std::mem::take(user);
                let password = std::mem::take(pass);

                start_credential_store(
                    services.sunshine_credentials.clone(),
                    username,
                    password,
                    sender.clone(),
                );
                state.overlay = Some(Overlay::Menu {
                    selected: StreamSetting::Resolution,
                });
            }
            _ => {}
        },
        Some(Overlay::Access) => match key.code {
            KeyCode::Esc | KeyCode::Char('q') => {
                state.overlay = Some(Overlay::Menu {
                    selected: StreamSetting::Resolution,
                });
            }
            KeyCode::Char('a') => {
                if let Some(node) = state.selected_remote() {
                    start_access_allow(
                        services.access.clone(),
                        AllowedController::new(
                            node.tailnet_node_id.clone(),
                            Some(node.name.clone()),
                            OffsetDateTime::now_utc(),
                            ControlCapability::ALL,
                        ),
                        sender.clone(),
                    );
                    state.notice = Some(format!("Allowed {} to control this device", node.name));
                } else {
                    state.error =
                        Some("Select a remote device first to grant it access".to_owned());
                }
            }
            KeyCode::Char('d') => {
                if let Some(node) = state.selected() {
                    let id = node.tailnet_node_id.clone();
                    start_access_revoke(services.access.clone(), id, sender.clone());
                    state.notice = Some(format!("Revoked access for {}", node.name));
                }
            }
            _ => {}
        },
        None => {}
    }
}

fn draw(frame: &mut Frame<'_>, state: &mut AppState) {
    let theme = state.theme.current().clone();

    frame.render_widget(
        Block::default().style(Style::default().bg(color(theme.background))),
        frame.area(),
    );

    let outer = centered_area(frame.area());
    let sections = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(11),
        Constraint::Length(3),
        Constraint::Length(3),
    ])
    .split(outer);

    draw_header(frame, sections[0], state, &theme);
    draw_content(frame, sections[1], state, &theme);
    draw_footer(frame, sections[2], state, &theme);
    draw_message(frame, sections[3], state, &theme);

    if let Some(overlay) = &state.overlay {
        draw_overlay(frame, overlay, state, &theme);
    }
}

fn popup_area(area: Rect, width: u16, height: u16) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn draw_overlay(frame: &mut Frame<'_>, overlay: &Overlay, state: &AppState, theme: &OmarchyTheme) {
    let (title, height, lines) = match overlay {
        Overlay::Menu { selected } => {
            let profile = &state.stream_profile;
            (
                "󰒓  Settings ",
                21u16,
                vec![
                    Line::default(),
                    settings_line(
                        "Display",
                        state.display_mode.label(),
                        *selected == StreamSetting::Display,
                        theme,
                    ),
                    settings_line(
                        "Resolution",
                        &format!("{}x{}", profile.width, profile.height),
                        *selected == StreamSetting::Resolution,
                        theme,
                    ),
                    settings_line(
                        "FPS",
                        &profile.fps.to_string(),
                        *selected == StreamSetting::Fps,
                        theme,
                    ),
                    settings_line(
                        "Bitrate",
                        &profile.bitrate_kbps.map_or_else(
                            || "Auto".to_owned(),
                            |value| format!("{} Mbps", value / 1000),
                        ),
                        *selected == StreamSetting::Bitrate,
                        theme,
                    ),
                    settings_line(
                        "Codec",
                        codec_label(profile.codec_preference),
                        *selected == StreamSetting::Codec,
                        theme,
                    ),
                    settings_line(
                        "Audio",
                        if profile.audio { "On" } else { "Off" },
                        *selected == StreamSetting::Audio,
                        theme,
                    ),
                    Line::default(),
                    property_line(
                        "Sunshine",
                        if state.sunshine_configured {
                            "configured"
                        } else {
                            "not configured"
                        },
                        theme,
                    ),
                    property_line(
                        "Allowlist",
                        &format!("{} identities", state.access_entries.len()),
                        theme,
                    ),
                    Line::default(),
                    Line::from(Span::styled(
                        "c Sunshine credentials    a access allowlist",
                        Style::default().fg(color(theme.muted)),
                    )),
                    Line::default(),
                    Line::from(Span::styled(
                        "↑↓ select    ←→ change",
                        Style::default().fg(color(theme.muted)),
                    )),
                    Line::from(Span::styled(
                        "Esc close",
                        Style::default().fg(color(theme.muted)),
                    )),
                ],
            )
        }
        Overlay::CredentialsUser { user } => (
            "󰌾  Sunshine credentials ",
            9,
            vec![
                Line::default(),
                Line::from(Span::styled(
                    "Enter the Sunshine admin username",
                    Style::default().fg(color(theme.muted)),
                )),
                Line::default(),
                input_line("Username", user, false, theme),
                Line::default(),
                Line::from(Span::styled(
                    "Enter next    Esc cancel",
                    Style::default().fg(color(theme.muted)),
                )),
            ],
        ),
        Overlay::CredentialsPass { pass, .. } => (
            "󰌾  Sunshine credentials ",
            9,
            vec![
                Line::default(),
                Line::from(Span::styled(
                    "Enter the Sunshine admin password",
                    Style::default().fg(color(theme.muted)),
                )),
                Line::default(),
                input_line("Password", pass, true, theme),
                Line::default(),
                Line::from(Span::styled(
                    "Enter save    Esc cancel",
                    Style::default().fg(color(theme.muted)),
                )),
            ],
        ),
        Overlay::Access => {
            let mut lines = vec![
                Line::default(),
                Line::from(Span::styled(
                    "Identities allowed to control this device:",
                    Style::default().fg(color(theme.muted)),
                )),
                Line::default(),
            ];

            if state.access_entries.is_empty() {
                lines.push(Line::from(Span::styled(
                    "  (empty, any Tailnet identity may connect, per Grants)",
                    Style::default().fg(color(theme.muted)),
                )));
            } else {
                for entry in &state.access_entries {
                    lines.push(Line::from(Span::styled(
                        format!("  ● {}", entry.label),
                        Style::default().fg(color(theme.foreground)),
                    )));
                }
            }

            lines.push(Line::default());
            lines.push(Line::from(Span::styled(
                "a allow selected device    d revoke    Esc back",
                Style::default().fg(color(theme.muted)),
            )));

            ("󱅣  Access allowlist ", 14, lines)
        }
    };

    let area = popup_area(frame.area(), 62, height);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .block(titled_panel(title, theme)),
        area,
    );
}

fn stream_profile(config: &Config) -> StreamProfile {
    let bitrate_kbps = (config.stream.bitrate_mbps > 0)
        .then(|| bitrate_kbps_from_mbps(config.stream.bitrate_mbps).ok())
        .flatten();

    let profile = StreamProfile {
        width: config.stream.width,
        height: config.stream.height,
        fps: config.stream.fps,
        codec_preference: config.stream.codec,
        audio: config.stream.audio,
        bitrate_kbps,
    };

    if profile.validate().is_ok() {
        profile
    } else {
        tracing::debug!("config.stream_profile_out_of_range");
        StreamProfile::default()
    }
}

fn save_stream_profile(state: &mut AppState) {
    let result = Config::load().and_then(|mut config| {
        config.stream.width = state.stream_profile.width;
        config.stream.height = state.stream_profile.height;
        config.stream.fps = state.stream_profile.fps;
        config.stream.codec = state.stream_profile.codec_preference;
        config.stream.audio = state.stream_profile.audio;
        config.stream.bitrate_mbps = state.stream_profile.bitrate_kbps.unwrap_or_default() / 1000;
        config.save()
    });

    match result {
        Ok(()) => {
            state.notice = Some("Stream settings saved".to_owned());
            state.error = None;
        }
        Err(error) => {
            state.error = Some(format!("Could not save stream settings: {error}"));
            state.notice = None;
        }
    }
}

fn change_display_mode(state: &mut AppState, direction: i8) {
    let current = DisplayMode::ALL
        .iter()
        .position(|mode| *mode == state.display_mode)
        .unwrap_or(0);
    let selected = cycle_index(current, DisplayMode::ALL.len(), direction);
    state.display_mode = DisplayMode::ALL[selected];
    save_display_mode(state);
}

fn save_display_mode(state: &mut AppState) {
    let result = Config::load().and_then(|mut config| {
        config.display.mode = state.display_mode;
        config.save()
    });

    match result {
        Ok(()) => {
            state.notice = Some("Display mode saved".to_owned());
            state.error = None;
        }
        Err(error) => {
            state.error = Some(format!("Could not save display mode: {error}"));
            state.notice = None;
        }
    }
}

fn change_stream_setting(profile: &mut StreamProfile, setting: StreamSetting, direction: i8) {
    match setting {
        StreamSetting::Display => {}
        StreamSetting::Resolution => {
            const VALUES: [(u32, u32); 4] = [(1280, 720), (1920, 1080), (2560, 1440), (3840, 2160)];
            let current = VALUES
                .iter()
                .position(|value| *value == (profile.width, profile.height))
                .unwrap_or(1);
            let selected = cycle_index(current, VALUES.len(), direction);
            (profile.width, profile.height) = VALUES[selected];
        }
        StreamSetting::Fps => {
            const VALUES: [u16; 4] = [30, 60, 90, 120];
            let current = VALUES
                .iter()
                .position(|value| *value == profile.fps)
                .unwrap_or(1);
            profile.fps = VALUES[cycle_index(current, VALUES.len(), direction)];
        }
        StreamSetting::Bitrate => {
            const VALUES: [Option<u32>; 6] = [
                None,
                Some(10_000),
                Some(20_000),
                Some(40_000),
                Some(80_000),
                Some(120_000),
            ];
            let current = VALUES
                .iter()
                .position(|value| *value == profile.bitrate_kbps)
                .unwrap_or(0);
            profile.bitrate_kbps = VALUES[cycle_index(current, VALUES.len(), direction)];
        }
        StreamSetting::Codec => {
            const VALUES: [CodecPreference; 4] = [
                CodecPreference::Auto,
                CodecPreference::H264,
                CodecPreference::Hevc,
                CodecPreference::Av1,
            ];
            let current = VALUES
                .iter()
                .position(|value| *value == profile.codec_preference)
                .unwrap_or(0);
            profile.codec_preference = VALUES[cycle_index(current, VALUES.len(), direction)];
        }
        StreamSetting::Audio => profile.audio = !profile.audio,
    }
}

fn cycle_index(current: usize, len: usize, direction: i8) -> usize {
    if direction < 0 {
        (current + len - 1) % len
    } else {
        (current + 1) % len
    }
}

fn codec_label(codec: CodecPreference) -> &'static str {
    match codec {
        CodecPreference::Auto => "Auto",
        CodecPreference::H264 => "H.264",
        CodecPreference::Hevc => "HEVC",
        CodecPreference::Av1 => "AV1",
    }
}

fn settings_line(label: &str, value: &str, selected: bool, theme: &OmarchyTheme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            if selected { "▸ " } else { "  " },
            Style::default().fg(color(theme.accent)),
        ),
        Span::styled(
            format!("{label:<11}"),
            Style::default().fg(color(theme.muted)),
        ),
        Span::styled(
            value.to_owned(),
            Style::default()
                .fg(color(if selected {
                    theme.accent
                } else {
                    theme.foreground
                }))
                .add_modifier(if selected {
                    Modifier::BOLD
                } else {
                    Modifier::empty()
                }),
        ),
    ])
}

fn input_line(label: &str, value: &str, masked: bool, theme: &OmarchyTheme) -> Line<'static> {
    let shown = if masked {
        "•".repeat(value.chars().count())
    } else {
        value.to_owned()
    };
    Line::from(vec![
        Span::styled(
            format!("{label:<11}"),
            Style::default().fg(color(theme.muted)),
        ),
        Span::styled(
            format!("{shown}▏"),
            Style::default()
                .fg(color(theme.accent))
                .add_modifier(Modifier::BOLD),
        ),
    ])
}

fn centered_area(area: Rect) -> Rect {
    let vertical = Layout::vertical([
        Constraint::Percentage(5),
        Constraint::Percentage(90),
        Constraint::Percentage(5),
    ])
    .split(area);
    let horizontal = Layout::horizontal([
        Constraint::Percentage(4),
        Constraint::Percentage(92),
        Constraint::Percentage(4),
    ])
    .split(vertical[1]);

    horizontal[1]
}

fn draw_header(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: &OmarchyTheme) {
    let mark = color(theme.accent);
    let ready = state
        .nodes
        .iter()
        .filter(|node| node.is_connectable())
        .count();
    let total = state.nodes.iter().filter(|node| !node.is_local).count();

    let status = if state.loading {
        Span::styled(
            "󰑓  scanning the tailnet…",
            Style::default().fg(color(theme.warning)),
        )
    } else if total == 0 {
        Span::styled(
            "󰤭  no remote devices yet",
            Style::default().fg(color(theme.muted)),
        )
    } else {
        Span::styled(
            format!("󰤨  {ready} of {total} devices ready"),
            Style::default().fg(color(theme.success)),
        )
    };

    let lines = vec![
        Line::from(Span::styled(
            "╭────╮",
            Style::default().fg(mark).add_modifier(Modifier::BOLD),
        )),
        Line::from(vec![
            Span::styled(
                "│ ╭────╮",
                Style::default().fg(mark).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled("OM", Style::default().fg(mark).add_modifier(Modifier::BOLD)),
            Span::styled(
                "DESKY",
                Style::default()
                    .fg(color(theme.foreground))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "╰─│    │",
                Style::default().fg(mark).add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            status,
        ]),
        Line::from(Span::styled(
            "  ╰────╯",
            Style::default().fg(mark).add_modifier(Modifier::BOLD),
        )),
    ];

    frame.render_widget(
        Paragraph::new(lines)
            .alignment(Alignment::Left)
            .block(panel(theme)),
        area,
    );
}

fn draw_content(frame: &mut Frame<'_>, area: Rect, state: &mut AppState, theme: &OmarchyTheme) {
    if state.detail_expanded {
        draw_detail_column(frame, area, state, theme);
        return;
    }

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(56), Constraint::Percentage(44)])
        .split(area);

    draw_devices(frame, columns[0], state, theme);
    draw_detail_column(frame, columns[1], state, theme);
}

fn draw_detail_column(
    frame: &mut Frame<'_>,
    area: Rect,
    state: &mut AppState,
    theme: &OmarchyTheme,
) {
    state.detail_rect = Some(area);
    draw_details(frame, area, state, theme);
}

fn wrap_text(text: &str, width: usize) -> Vec<String> {
    if width == 0 {
        return vec![text.to_owned()];
    }

    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                push_word(&mut lines, &mut current, word, width);
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(std::mem::take(&mut current));
                push_word(&mut lines, &mut current, word, width);
            }
        }

        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }

    lines
}

fn push_word(lines: &mut Vec<String>, current: &mut String, word: &str, width: usize) {
    if word.chars().count() <= width {
        *current = word.to_owned();
        return;
    }

    let mut chunk = String::new();
    for character in word.chars() {
        if chunk.chars().count() == width {
            lines.push(std::mem::take(&mut chunk));
        }
        chunk.push(character);
    }

    *current = chunk;
}

fn draw_devices(frame: &mut Frame<'_>, area: Rect, state: &mut AppState, theme: &OmarchyTheme) {
    if state.loading || state.nodes.is_empty() {
        frame.render_widget(
            Paragraph::new(empty_state_lines(state, theme))
                .alignment(Alignment::Center)
                .block(titled_panel("󰇄  Devices", theme)),
            area,
        );
        return;
    }

    let items = state
        .nodes
        .iter()
        .map(|node| device_item(node, theme))
        .collect::<Vec<_>>();
    let list = List::new(items)
        .block(titled_panel("󰇄  Devices", theme).padding(Padding::new(1, 1, 1, 0)))
        .highlight_symbol(" ")
        .highlight_style(
            Style::default()
                .fg(color(theme.foreground))
                .bg(color(theme.selection))
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut state.list);
    draw_selection(frame, area, state, theme);
}

const DEVICE_ITEM_HEIGHT: u16 = 4;

fn draw_selection(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: &OmarchyTheme) {
    let Some(selected) = state.list.selected() else {
        return;
    };

    let offset = state.list.offset();
    if selected < offset {
        return;
    }

    let bar_x = area.x + 2;
    let inner_top = area.y + 2;
    let inner_bottom = area.y + area.height.saturating_sub(1);
    let rel = (selected - offset) as u16;
    let y_start = inner_top + rel * DEVICE_ITEM_HEIGHT;

    let accent = color(theme.accent);
    let selection = color(theme.selection);
    let buffer = frame.buffer_mut();

    for row in 0..DEVICE_ITEM_HEIGHT {
        let y = y_start + row;
        if y >= inner_bottom {
            break;
        }

        if let Some(cell) = buffer.cell_mut((bar_x, y)) {
            cell.set_symbol("▌");
            cell.set_fg(accent);
            cell.set_bg(selection);
        }
    }
}

fn device_item(node: &DiscoveredNode, theme: &OmarchyTheme) -> ListItem<'static> {
    let name = if node.is_local {
        format!("{}  (this device)", node.name)
    } else {
        node.name.clone()
    };

    let foreground = if node.is_local {
        theme.muted
    } else {
        theme.foreground
    };

    let latency = node
        .latency_ms
        .map_or_else(|| "—".to_owned(), |value| format!("{value} ms"));

    ListItem::new(vec![
        Line::default(),
        Line::from(vec![
            Span::styled(
                format!("  {}  ", device_glyph(node)),
                status_style(node, theme),
            ),
            Span::styled(
                name,
                Style::default()
                    .fg(color(foreground))
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(
                format!("{}  {:<12}", status_icon(node), status_label(node)),
                status_style(node, theme),
            ),
            Span::styled(
                format!("󰅐 {latency:<9}"),
                Style::default().fg(color(theme.muted)),
            ),
            Span::styled(
                format!(
                    "{} {}",
                    connection_icon(node.connection),
                    connection_label(node.connection)
                ),
                Style::default().fg(color(theme.muted)),
            ),
        ]),
        Line::default(),
    ])
}

fn device_glyph(node: &DiscoveredNode) -> &'static str {
    if node.is_local { "󰋜" } else { "󰍹" }
}

fn status_icon(node: &DiscoveredNode) -> &'static str {
    if node.is_local {
        return "󰐾";
    }

    match (
        node.status,
        node.blockers.first().map(|blocker| blocker.code),
    ) {
        (NodeStatus::Ready, _) => "󰄬",
        (NodeStatus::Offline, _) => "󰅖",
        (NodeStatus::Blocked, Some(BlockerCode::Incompatible)) => "󰀦",
        (NodeStatus::Blocked, _) => "󰌾",
        (NodeStatus::Unavailable, _) => "󰋗",
    }
}

fn connection_icon(connection: ConnectionKind) -> &'static str {
    match connection {
        ConnectionKind::Direct => "󰌘",
        ConnectionKind::Relay => "󰑩",
        ConnectionKind::Unknown => "󰤭",
    }
}

fn draw_details(frame: &mut Frame<'_>, area: Rect, state: &mut AppState, theme: &OmarchyTheme) {
    let inner_width = area.width.saturating_sub(4).max(1) as usize;
    let mut lines = state.selected().map_or_else(
        || {
            vec![
                Line::default(),
                Line::from(Span::styled(
                    "Select a device",
                    Style::default().fg(color(theme.muted)),
                )),
            ]
        },
        |node| detail_lines(node, theme, inner_width),
    );

    let available = area.height.saturating_sub(2) as usize;

    state.detail_overflow = lines.len() > available;
    if state.detail_overflow && !state.detail_expanded {
        lines.truncate(available.saturating_sub(1));
        lines.push(Line::from(Span::styled(
            "▾ click to expand",
            Style::default()
                .fg(color(theme.muted))
                .add_modifier(Modifier::BOLD),
        )));
    }

    let paragraph = Paragraph::new(lines).block(titled_panel("󰔚  Overview", theme));
    if state.detail_expanded {
        frame.render_widget(paragraph.wrap(ratatui::widgets::Wrap { trim: false }), area);
    } else {
        frame.render_widget(paragraph, area);
    }
}

fn detail_lines(node: &DiscoveredNode, theme: &OmarchyTheme, width: usize) -> Vec<Line<'static>> {
    let capabilities = if node.capabilities.as_slice().is_empty() {
        "Not advertised".to_owned()
    } else {
        node.capabilities.as_slice().join(", ")
    };

    let mut lines = vec![Line::default()];
    lines.extend(styled_wrapped_lines(
        &node.name,
        width,
        Style::default()
            .fg(color(theme.foreground))
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(styled_wrapped_lines(
        &node.address.to_string(),
        width,
        Style::default().fg(color(theme.foreground)),
    ));
    lines.push(Line::default());
    lines.extend(wrapped_property_lines(
        "Status",
        status_label(node),
        width,
        theme,
    ));
    lines.extend(wrapped_property_lines(
        "Link",
        connection_label(node.connection),
        width,
        theme,
    ));
    lines.extend(wrapped_property_lines(
        "Latency",
        &node
            .latency_ms
            .map_or_else(|| "—".to_owned(), |value| format!("{value} ms")),
        width,
        theme,
    ));
    lines.extend(wrapped_property_lines(
        "Omarchy",
        node.omarchy_version.as_deref().unwrap_or("—"),
        width,
        theme,
    ));
    lines.extend(wrapped_property_lines(
        "Agent",
        node.agent_version.as_deref().unwrap_or("—"),
        width,
        theme,
    ));
    lines.push(Line::default());
    lines.extend(styled_wrapped_lines(
        "CAPABILITIES",
        width,
        Style::default()
            .fg(color(theme.muted))
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(styled_wrapped_lines(
        &capabilities,
        width,
        Style::default().fg(color(theme.foreground)),
    ));

    lines
}

fn styled_wrapped_lines(text: &str, width: usize, style: Style) -> Vec<Line<'static>> {
    wrap_text(text, width)
        .into_iter()
        .map(|line| Line::from(Span::styled(line, style)))
        .collect()
}

fn wrapped_property_lines(
    label: &str,
    value: &str,
    width: usize,
    theme: &OmarchyTheme,
) -> Vec<Line<'static>> {
    let label_width = 11usize.min(width);
    let value_width = width.saturating_sub(label_width).max(1);
    let wrapped = wrap_text(value, value_width);

    wrapped
        .into_iter()
        .enumerate()
        .map(|(index, value)| {
            Line::from(vec![
                Span::styled(
                    if index == 0 {
                        format!("{label:<label_width$}")
                    } else {
                        " ".repeat(label_width)
                    },
                    Style::default().fg(color(theme.muted)),
                ),
                Span::styled(value, Style::default().fg(color(theme.foreground))),
            ])
        })
        .collect()
}

fn property_line(label: &str, value: &str, theme: &OmarchyTheme) -> Line<'static> {
    Line::from(vec![
        Span::styled(
            format!("{label:<11}"),
            Style::default().fg(color(theme.muted)),
        ),
        Span::styled(
            value.to_owned(),
            Style::default().fg(color(theme.foreground)),
        ),
    ])
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: &OmarchyTheme) {
    let mut spans = vec![
        key("↑↓", theme),
        label("  navigate    ", theme),
        key("r", theme),
        label("  refresh    ", theme),
    ];

    if state.selected_remote().is_some() && state.local_readiness == LocalReadiness::Ready {
        spans.extend([key("󰌑 Enter", theme), label("  connect    ", theme)]);
    }

    spans.extend([
        key("s", theme),
        label("  settings    ", theme),
        key("q", theme),
        label("  quit", theme),
    ]);

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center)
            .block(panel(theme)),
        area,
    );
}

fn draw_message(frame: &mut Frame<'_>, area: Rect, state: &AppState, theme: &OmarchyTheme) {
    let line = state
        .message()
        .map(|(kind, message)| message_line(kind, message, theme))
        .unwrap_or_default();

    frame.render_widget(Paragraph::new(line).block(panel(theme)), area);
}

fn message_line(kind: MessageKind, message: &str, theme: &OmarchyTheme) -> Line<'static> {
    let (title, accent) = match kind {
        MessageKind::Notice => (" INFO ", theme.success),
        MessageKind::Error => (" ERROR ", theme.error),
    };

    Line::from(vec![
        Span::styled(
            title,
            Style::default()
                .fg(color(theme.background))
                .bg(color(accent))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" | {message}"), Style::default().fg(color(accent))),
    ])
}

fn key(value: &'static str, theme: &OmarchyTheme) -> Span<'static> {
    Span::styled(
        format!(" {value} "),
        Style::default()
            .fg(color(theme.background))
            .bg(color(theme.accent))
            .add_modifier(Modifier::BOLD),
    )
}

fn label(value: &'static str, theme: &OmarchyTheme) -> Span<'static> {
    Span::styled(value, Style::default().fg(color(theme.muted)))
}

fn panel(theme: &OmarchyTheme) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color(theme.selection)))
        .style(
            Style::default()
                .fg(color(theme.foreground))
                .bg(color(theme.background)),
        )
        .padding(Padding::horizontal(1))
}

fn titled_panel(title: &'static str, theme: &OmarchyTheme) -> Block<'static> {
    panel(theme).title(Span::styled(
        title,
        Style::default()
            .fg(color(theme.accent))
            .add_modifier(Modifier::BOLD),
    ))
}

fn empty_state_lines(state: &AppState, theme: &OmarchyTheme) -> Vec<Line<'static>> {
    if state.loading {
        return vec![
            Line::default(),
            Line::from(Span::styled(
                "󰃳  Scanning the tailnet for Omdesky devices…",
                Style::default().fg(color(theme.accent)),
            )),
        ];
    }

    if let Some(error) = &state.error {
        return vec![
            Line::default(),
            Line::from(Span::styled(
                "󱊦  Discovery failed",
                Style::default()
                    .fg(color(theme.error))
                    .add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                error.clone(),
                Style::default().fg(color(theme.muted)),
            )),
            Line::default(),
            Line::from("Press r to retry"),
        ];
    }

    vec![
        Line::default(),
        Line::from(Span::styled(
            "󰇄  No compatible devices found",
            Style::default().fg(color(theme.foreground)),
        )),
        Line::from(Span::styled(
            "Start omdesky-agent on another Omarchy device",
            Style::default().fg(color(theme.muted)),
        )),
    ]
}

fn status_label(node: &DiscoveredNode) -> &'static str {
    match (
        node.status,
        node.blockers.first().map(|blocker| blocker.code),
    ) {
        (NodeStatus::Ready, _) => "READY",
        (NodeStatus::Offline, _) => "OFFLINE",
        (NodeStatus::Unavailable, _) => "UNKNOWN",
        (NodeStatus::Blocked, Some(BlockerCode::Incompatible)) => "INCOMPATIBLE",
        (NodeStatus::Blocked, Some(BlockerCode::Denied)) => "DENIED",
        (NodeStatus::Blocked, Some(BlockerCode::NeedsAccess)) => "NEEDS ACCESS",
        (NodeStatus::Blocked, None) => "BLOCKED",
    }
}

fn connection_label(connection: ConnectionKind) -> &'static str {
    match connection {
        ConnectionKind::Direct => "DIRECT",
        ConnectionKind::Relay => "RELAY",
        ConnectionKind::Unknown => "—",
    }
}

fn status_style(node: &DiscoveredNode, theme: &OmarchyTheme) -> Style {
    let status_color = if node.is_local {
        theme.muted
    } else {
        match (
            node.status,
            node.blockers.first().map(|blocker| blocker.code),
        ) {
            (NodeStatus::Ready, _) => theme.success,
            (NodeStatus::Blocked, Some(BlockerCode::NeedsAccess))
            | (NodeStatus::Unavailable, _) => theme.warning,
            (NodeStatus::Blocked, _) | (NodeStatus::Offline, _) => theme.error,
        }
    };

    Style::default().fg(color(status_color))
}

fn color(value: Rgb) -> Color {
    Color::Rgb(value.red, value.green, value.blue)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_core::NodeCapabilities;
    use ratatui::{Terminal, backend::TestBackend};
    use std::net::{IpAddr, Ipv4Addr};

    #[test]
    fn test_wrap_text_breaks_on_width_and_preserves_words() {
        let wrapped = wrap_text("the quick brown fox jumps", 10);
        assert!(wrapped.iter().all(|line| line.chars().count() <= 10));
        assert_eq!(wrapped.join(" "), "the quick brown fox jumps");
    }

    #[test]
    fn test_wrap_text_hard_splits_long_tokens() {
        let wrapped = wrap_text("SUNSHINE_API_UNAVAILABLE", 8);
        assert!(wrapped.len() >= 3);
        assert!(wrapped.iter().all(|line| line.chars().count() <= 8));
    }

    #[test]
    fn test_wrap_text_honours_existing_newlines() {
        let wrapped = wrap_text("line one\nline two", 40);
        assert_eq!(wrapped, vec!["line one".to_owned(), "line two".to_owned()]);
    }

    #[test]
    fn test_overview_content_wraps_all_long_values() {
        let mut selected = node("workstation-with-a-name-that-does-not-fit");
        selected.capabilities = NodeCapabilities::new([
            "desktop.stream-host".to_owned(),
            "desktop.remote-input".to_owned(),
        ]);
        let mut state = state();
        apply_refresh(&mut state, Ok(vec![selected]));
        let theme = state.theme.current().clone();

        let lines = detail_lines(state.selected().expect("selected node"), &theme, 18);

        assert!(lines.len() > 12);
        assert!(lines.iter().all(|line| line.width() <= 18));
    }

    #[test]
    fn test_selected_device_background_and_accent_have_equal_height() {
        let mut state = state();
        apply_refresh(&mut state, Ok(vec![node("workstation"), node("notebook")]));
        let theme = state.theme.current().clone();
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| draw_devices(frame, frame.area(), &mut state, &theme))
            .expect("draw devices");

        let buffer = terminal.backend().buffer();
        for row in 2..2 + DEVICE_ITEM_HEIGHT {
            assert_eq!(
                buffer.cell((2, row)).expect("accent").bg,
                color(theme.selection)
            );
            assert_eq!(
                buffer.cell((3, row)).expect("background").bg,
                color(theme.selection)
            );
        }
        assert_eq!(
            buffer.cell((8, 3)).expect("centered device name").symbol(),
            "w"
        );
        assert_eq!(buffer.cell((3, 2)).expect("top spacing").symbol(), " ");
        assert_eq!(buffer.cell((3, 5)).expect("bottom spacing").symbol(), " ");
        assert_eq!(
            buffer
                .cell((3, 2 + DEVICE_ITEM_HEIGHT))
                .expect("next device")
                .bg,
            color(theme.background)
        );
    }

    #[test]
    fn test_header_keeps_complete_logo_and_centers_title() {
        let mut state = state();
        apply_refresh(&mut state, Ok(vec![node("workstation")]));
        let theme = state.theme.current().clone();
        let backend = TestBackend::new(60, 6);
        let mut terminal = Terminal::new(backend).expect("test terminal");

        terminal
            .draw(|frame| draw_header(frame, frame.area(), &state, &theme))
            .expect("draw header");

        let buffer = terminal.backend().buffer();
        let top = (0..60)
            .map(|column| buffer.cell((column, 1)).expect("top cell").symbol())
            .collect::<String>();
        let middle = (0..60)
            .map(|column| buffer.cell((column, 2)).expect("middle cell").symbol())
            .collect::<String>();
        let lower_middle = (0..60)
            .map(|column| {
                buffer
                    .cell((column, 3))
                    .expect("lower middle cell")
                    .symbol()
            })
            .collect::<String>();
        let bottom = (0..60)
            .map(|column| buffer.cell((column, 4)).expect("bottom cell").symbol())
            .collect::<String>();

        assert!(top.contains("╭────╮"));
        assert!(middle.contains("│ ╭────╮   OMDESKY"));
        assert!(lower_middle.contains("╰─│    │"));
        assert!(lower_middle.contains("1 of 1 devices ready"));
        assert!(bottom.contains("╰────╯"));
    }

    #[test]
    fn test_message_line_uses_info_badge_and_success_colors() {
        let theme = OmarchyTheme::default();

        let line = message_line(MessageKind::Notice, "Connected", &theme);

        assert_eq!(line.to_string(), " INFO  | Connected");
        assert_eq!(line.spans[0].style.fg, Some(color(theme.background)));
        assert_eq!(line.spans[0].style.bg, Some(color(theme.success)));
        assert_eq!(line.spans[1].style.fg, Some(color(theme.success)));
    }

    #[test]
    fn test_message_line_uses_error_badge_and_error_colors() {
        let theme = OmarchyTheme::default();

        let line = message_line(MessageKind::Error, "Connection failed", &theme);

        assert_eq!(line.to_string(), " ERROR  | Connection failed");
        assert_eq!(line.spans[0].style.fg, Some(color(theme.background)));
        assert_eq!(line.spans[0].style.bg, Some(color(theme.error)));
        assert_eq!(line.spans[1].style.fg, Some(color(theme.error)));
    }

    #[test]
    fn test_user_error_adds_context_without_exposing_details() {
        let error = PortError::new(
            "AGENT_UNREACHABLE",
            "request to http://100.64.0.7:48155 failed",
            true,
        );

        let message = user_error("The connection could not be completed.", &error);

        assert_eq!(
            message,
            "The connection could not be completed. This device could not be reached. Check that it is online and connected to Tailscale."
        );
        assert!(!message.contains("100.64.0.7"));
    }

    #[test]
    fn test_stream_exit_message_hides_process_exit_code() {
        assert_eq!(
            stream_exit_message("workstation", 1),
            "The stream from workstation ended unexpectedly. You can try reconnecting."
        );
        assert!(!stream_exit_message("workstation", 1).contains("exit 1"));
    }

    #[test]
    fn test_stream_setting_cycles_through_supported_values() {
        let mut profile = StreamProfile::default();

        change_stream_setting(&mut profile, StreamSetting::Bitrate, 1);
        change_stream_setting(&mut profile, StreamSetting::Fps, 1);
        change_stream_setting(&mut profile, StreamSetting::Codec, 1);
        change_stream_setting(&mut profile, StreamSetting::Audio, 1);

        assert_eq!(profile.bitrate_kbps, Some(10_000));
        assert_eq!(profile.fps, 90);
        assert_eq!(profile.codec_preference, CodecPreference::H264);
        assert!(!profile.audio);
    }

    fn node(name: &str) -> DiscoveredNode {
        DiscoveredNode {
            tailnet_node_id: format!("tail-{name}"),
            name: name.to_owned(),
            address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)),
            status: NodeStatus::Ready,
            blockers: Vec::new(),
            connection: ConnectionKind::Direct,
            latency_ms: Some(4),
            agent_version: Some("0.1.0".to_owned()),
            omarchy_version: Some("4.0.1".to_owned()),
            capabilities: NodeCapabilities::new(["desktop.stream-host".to_owned()]),
            is_local: false,
        }
    }

    fn state() -> AppState {
        AppState::new(ThemeWatcher::new(PathBuf::from("/nonexistent")))
    }

    #[test]
    fn test_a_notice_disappears_after_its_lifetime() {
        let mut state = state();
        let start = Instant::now();
        state.notice = Some("Stream settings saved".to_owned());

        state.expire_message(start);
        state.expire_message(start + NOTICE_LIFETIME - Duration::from_millis(1));

        assert!(state.notice.is_some());

        state.expire_message(start + NOTICE_LIFETIME);

        assert!(state.notice.is_none());
        assert!(state.message().is_none());
    }

    #[test]
    fn test_an_error_stays_longer_than_a_notice() {
        let mut state = state();
        let start = Instant::now();
        state.error = Some("The devices could not be paired.".to_owned());

        state.expire_message(start);
        state.expire_message(start + NOTICE_LIFETIME);

        assert!(state.error.is_some());

        state.expire_message(start + ERROR_LIFETIME);

        assert!(state.error.is_none());
    }

    #[test]
    fn test_an_expired_error_does_not_reveal_an_older_notice() {
        let mut state = state();
        let start = Instant::now();
        state.notice = Some("Connected to desk.".to_owned());
        state.error = Some("The stream ended.".to_owned());

        state.expire_message(start);
        state.expire_message(start + ERROR_LIFETIME);

        assert!(state.message().is_none());
    }

    #[test]
    fn test_a_new_message_restarts_the_lifetime() {
        let mut state = state();
        let start = Instant::now();
        state.notice = Some("Stream settings saved".to_owned());

        state.expire_message(start);

        state.notice = Some("Display mode saved".to_owned());
        state.expire_message(start + NOTICE_LIFETIME);
        state.expire_message(start + NOTICE_LIFETIME + NOTICE_LIFETIME / 2);

        assert_eq!(state.notice.as_deref(), Some("Display mode saved"));
    }

    #[test]
    fn test_a_progress_message_stays_while_connecting() {
        let mut state = state();
        let start = Instant::now();
        state.activity = Activity::Connecting;
        state.notice = Some("Checking connection to desk…".to_owned());

        state.expire_message(start);
        state.expire_message(start + ERROR_LIFETIME * 3);

        assert!(state.notice.is_some());

        state.activity = Activity::Idle;
        state.expire_message(start + ERROR_LIFETIME * 3 + NOTICE_LIFETIME);

        assert!(state.notice.is_none());
    }

    #[test]
    fn test_loading_state_is_visible_before_discovery_finishes() {
        let mut state = state();
        state.loading = true;
        let theme = state.theme.current().clone();
        let lines = empty_state_lines(&state, &theme);

        assert!(lines[1].to_string().contains("Scanning"));
    }

    #[test]
    fn test_session_lifecycle_requires_terminal_reset() {
        let started = AsyncMessage::SessionStarted {
            generation: 1,
            node_id: "tail-workstation".to_owned(),
            node_name: "workstation".to_owned(),
            moonlight_pid: Some(4242),
        };
        let ended = AsyncMessage::SessionEnded {
            generation: 1,
            result: Ok("disconnected".to_owned()),
            local_failure: false,
        };

        assert!(requires_terminal_reset(&started));
        assert!(requires_terminal_reset(&ended));
    }

    #[test]
    fn test_refresh_selects_first_device() {
        let mut state = state();
        apply_refresh(&mut state, Ok(vec![node("workstation")]));

        assert_eq!(state.list.selected(), Some(0));
        assert_eq!(
            state.selected().map(|node| node.name.as_str()),
            Some("workstation")
        );
    }

    #[test]
    fn test_navigation_wraps_around_device_list() {
        let mut state = state();
        apply_refresh(&mut state, Ok(vec![node("workstation"), node("notebook")]));

        state.select_previous();
        assert_eq!(state.list.selected(), Some(1));

        state.select_next();
        assert_eq!(state.list.selected(), Some(0));
    }

    #[test]
    fn test_session_action_starts_without_active_session() {
        assert_eq!(
            session_action(None, "tail-workstation"),
            SessionAction::Start
        );
    }

    #[test]
    fn test_session_action_focuses_active_device() {
        assert_eq!(
            session_action(Some("tail-workstation"), "tail-workstation"),
            SessionAction::Focus
        );
    }

    #[test]
    fn test_session_action_switches_to_another_device() {
        assert_eq!(
            session_action(Some("tail-workstation"), "tail-notebook"),
            SessionAction::Switch
        );
    }

    #[test]
    fn test_local_device_is_visible_but_cannot_connect() {
        let mut local = node("omarchy");
        local.is_local = true;
        let mut state = state();
        state.local_readiness = LocalReadiness::Ready;

        apply_refresh(&mut state, Ok(vec![local]));

        assert!(state.selected().is_some());
        assert!(state.selected_remote().is_none());
        assert_eq!(
            state.unavailable_reason().as_deref(),
            Some("This device cannot connect to itself")
        );
    }

    #[test]
    fn test_local_readiness_blocks_connection_before_device_validation() {
        let mut state = state();
        apply_refresh(&mut state, Ok(vec![node("workstation")]));
        state.local_readiness =
            LocalReadiness::Blocked("The local agent is not running.".to_owned());

        assert_eq!(
            state.unavailable_reason().as_deref(),
            Some("The local agent is not running.")
        );
    }

    #[test]
    fn test_refresh_preserves_local_readiness_error() {
        let mut state = state();
        state.local_readiness =
            LocalReadiness::Blocked("The local agent is not running.".to_owned());
        state.error = Some("The local agent is not running.".to_owned());

        apply_refresh(&mut state, Ok(vec![node("workstation")]));

        assert_eq!(
            state.error.as_deref(),
            Some("The local agent is not running.")
        );
    }

    #[test]
    fn test_discovery_error_is_actionable_in_tui() {
        let mut state = state();
        apply_refresh(&mut state, Err("TAILSCALE_NOT_CONNECTED".to_owned()));
        let theme = state.theme.current().clone();
        let lines = empty_state_lines(&state, &theme);

        assert!(lines[2].to_string().contains("TAILSCALE_NOT_CONNECTED"));
        assert!(lines[4].to_string().contains('r'));
    }
}
