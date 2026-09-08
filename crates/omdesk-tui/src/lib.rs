use crossterm::event::{
    self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEvent, KeyEventKind,
    MouseButton, MouseEventKind,
};
use omdesk_application::{
    ports::{AccessStore, AgentEndpoint, AllowedController, MeshNetwork},
    services::{ConnectNode, ConnectRequest, DiscoverNodes, DiscoveredNode},
};
use omdesk_core::{CodecPreference, ConnectionKind, InputMode, NodeStatus, StreamProfile};
use omdesk_platform::{
    access::FileAccessStore,
    agent_client::HttpAgentClient,
    config::{Config, access_path, state_dir, sunshine_credentials_path},
    input::HyprlandSessionKeybinds,
    moonlight::MoonlightAdapter,
    omarchy::OmarchyNotificationAdapter,
    process::TokioCommandRunner,
    sunshine::store_credentials,
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
use std::{env, path::PathBuf, sync::Arc, time::Duration};
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
    access: Arc<FileAccessStore>,
    sunshine_credentials_path: PathBuf,
    agent_port: u16,
    client_name: String,
}

impl Services {
    fn new() -> anyhow::Result<Self> {
        let config = Config::load()?;
        let runner = Arc::new(TokioCommandRunner);
        let mesh = Arc::new(TailscaleAdapter::new(runner.clone()));
        let agent = Arc::new(HttpAgentClient::new());

        let _ = state_dir();

        Ok(Self {
            discovery: Arc::new(DiscoverNodes::new(
                mesh,
                agent.clone(),
                config.network.agent_port,
            )),
            agent,
            stream: Arc::new(MoonlightAdapter::new(runner)),
            notifications: Arc::new(OmarchyNotificationAdapter::default()),
            access: Arc::new(FileAccessStore::new(access_path()?)),
            sunshine_credentials_path: sunshine_credentials_path()?,
            agent_port: config.network.agent_port,
            client_name: env::var("HOSTNAME").unwrap_or_else(|_| "desklink".to_owned()),
        })
    }

    fn sunshine_configured(&self) -> bool {
        self.sunshine_credentials_path.exists()
    }

    fn connect_service(&self) -> ConnectNode {
        ConnectNode::new(
            self.agent.clone(),
            self.stream.clone(),
            Arc::new(HyprlandSessionKeybinds::new(Arc::new(TokioCommandRunner))),
            self.notifications.clone(),
            self.client_name.clone(),
        )
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum MessageKind {
    Notice,
    Error,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StreamSetting {
    Resolution,
    Fps,
    Bitrate,
    Codec,
    Audio,
}

impl StreamSetting {
    const ALL: [Self; 5] = [
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

enum AsyncMessage {
    Discovery(DiscoveryResult),
    Activity(Result<String, String>),
    Access(Result<Vec<AccessRow>, String>),
}

struct AppState {
    nodes: Vec<DiscoveredNode>,
    list: ListState,
    theme: ThemeWatcher,
    loading: bool,
    activity: Activity,
    notice: Option<String>,
    error: Option<String>,
    overlay: Option<Overlay>,
    sunshine_configured: bool,
    access_entries: Vec<AccessRow>,
    stream_profile: StreamProfile,
    detail_expanded: bool,
    detail_overflow: bool,
    detail_rect: Option<Rect>,
}

impl AppState {
    fn new(theme: ThemeWatcher) -> Self {
        Self {
            nodes: Vec::new(),
            list: ListState::default(),
            theme,
            loading: false,
            activity: Activity::Idle,
            notice: None,
            error: None,
            overlay: None,
            sunshine_configured: false,
            access_entries: Vec::new(),
            stream_profile: StreamProfile::default(),
            detail_expanded: false,
            detail_overflow: false,
            detail_rect: None,
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
            .filter(|node| !node.is_local && node.status == NodeStatus::Ready)
            .cloned()
    }

    fn unavailable_reason(&self) -> Option<&'static str> {
        let node = self.selected()?;
        if node.is_local {
            Some("This device cannot connect to itself")
        } else if node.status != NodeStatus::Ready {
            Some("This device is not ready")
        } else {
            None
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
            .map_err(|error| error.to_string());
        let _ = sender.send(AsyncMessage::Discovery(result)).await;
    });
}

fn start_connect(services: &Services, node: DiscoveredNode, sender: mpsc::Sender<AsyncMessage>) {
    let service = services.connect_service();
    let endpoint = AgentEndpoint {
        address: node.address,
        port: services.agent_port,
    };
    let agent_port = services.agent_port;
    let profile = Config::load()
        .map(|config| stream_profile(&config))
        .unwrap_or_else(|_| StreamProfile::default());
    let input_mode = InputMode::Remote;
    tokio::spawn(async move {
        let controller_endpoint = local_agent_endpoint(agent_port)
            .await
            .unwrap_or_else(|_| endpoint.clone());
        let result = service
            .execute(ConnectRequest {
                endpoint,
                controller_endpoint,
                profile,
                fullscreen: true,
                input_mode,
                focus_workspace: None,
                focus_window: None,
                auto_pair: true,
            })
            .await
            .map(|exit| format!("Disconnected from {} (exit {exit})", node.name))
            .map_err(|error| error.to_string());
        let _ = sender.send(AsyncMessage::Activity(result)).await;
    });
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
            .map_err(|error| error.to_string());
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
                .send(AsyncMessage::Access(Err(error.to_string())))
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
                .send(AsyncMessage::Access(Err(error.to_string())))
                .await;
            return;
        }
        start_access_list(access, sender);
    });
}

fn requires_terminal_reset(message: &AsyncMessage) -> bool {
    matches!(message, AsyncMessage::Activity(_))
}

fn apply_message(state: &mut AppState, message: AsyncMessage) {
    match message {
        AsyncMessage::Discovery(result) => apply_refresh(state, result),
        AsyncMessage::Activity(result) => {
            state.activity = Activity::Idle;
            state.detail_expanded = false;
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
        AsyncMessage::Access(result) => match result {
            Ok(entries) => state.access_entries = entries,
            Err(error) => {
                state.detail_expanded = false;
                state.error = Some(error);
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
            state.error = None;
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
    state.sunshine_configured = services.sunshine_configured();
    state.stream_profile = Config::load()
        .map(|config| stream_profile(&config))
        .unwrap_or_else(|_| StreamProfile::default());
    start_refresh(services.discovery.clone(), sender.clone());

    loop {
        state.theme.refresh();

        while let Ok(message) = receiver.try_recv() {
            let reset_terminal = requires_terminal_reset(&message);
            apply_message(state, message);
            if reset_terminal {
                terminal.clear()?;
                terminal.autoresize()?;
            }
        }

        terminal.draw(|frame| draw(frame, state))?;

        if !event::poll(Duration::from_millis(80))? {
            continue;
        }

        match event::read()? {
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                if handle_key(state, &services, &sender, key) {
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

fn handle_key(
    state: &mut AppState,
    services: &Services,
    sender: &mpsc::Sender<AsyncMessage>,
    key: KeyEvent,
) -> bool {
    if state.overlay.is_some() {
        handle_overlay_key(state, services, sender, key);
        return false;
    }

    if state.activity != Activity::Idle {
        return false;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => return true,
        KeyCode::Down | KeyCode::Char('j') => state.select_next(),
        KeyCode::Up | KeyCode::Char('k') => state.select_previous(),
        KeyCode::Char('r') if !state.loading => {
            state.loading = true;
            state.error = None;
            state.notice = None;
            start_refresh(services.discovery.clone(), sender.clone());
        }
        KeyCode::Char('s') => {
            state.sunshine_configured = services.sunshine_configured();
            state.stream_profile = Config::load()
                .map(|config| stream_profile(&config))
                .unwrap_or_else(|_| StreamProfile::default());
            state.overlay = Some(Overlay::Menu {
                selected: StreamSetting::Resolution,
            });
        }
        KeyCode::Enter => {
            if let Some(reason) = state.unavailable_reason() {
                state.error = Some(reason.to_owned());
                state.notice = None;
            } else if let Some(node) = state.selected_remote() {
                state.activity = Activity::Connecting;
                state.error = None;
                state.notice = Some(format!("Connecting to {}…", node.name));
                start_connect(services, node, sender.clone());
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
                change_stream_setting(&mut state.stream_profile, *selected, -1);
                save_stream_profile(state);
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Enter => {
                change_stream_setting(&mut state.stream_profile, *selected, 1);
                save_stream_profile(state);
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
                let result = store_credentials(&services.sunshine_credentials_path, user, pass);
                match result {
                    Ok(()) => {
                        state.sunshine_configured = true;
                        state.notice = Some("Sunshine credentials saved".to_owned());
                        state.error = None;
                    }
                    Err(error) => {
                        state.error = Some(format!("Could not save credentials: {error}"));
                        state.notice = None;
                    }
                }
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
                        AllowedController {
                            tailnet_node_id: node.tailnet_node_id.clone(),
                            label: Some(node.name.clone()),
                            added_at: OffsetDateTime::now_utc(),
                        },
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
        Constraint::Length(3),
        Constraint::Min(14),
        Constraint::Length(3),
    ])
    .split(outer);

    draw_header(frame, sections[0], &theme);
    draw_content(frame, sections[1], state, &theme);
    draw_footer(frame, sections[2], state, &theme);

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
                " Settings ",
                20u16,
                vec![
                    Line::default(),
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
            " Sunshine credentials ",
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
            " Sunshine credentials ",
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
            (" Access allowlist ", 14, lines)
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
    StreamProfile {
        width: config.stream.width,
        height: config.stream.height,
        fps: config.stream.fps,
        codec_preference: config.stream.codec,
        audio: config.stream.audio,
        bitrate_kbps: (config.stream.bitrate_mbps > 0).then_some(config.stream.bitrate_mbps * 1000),
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

fn change_stream_setting(profile: &mut StreamProfile, setting: StreamSetting, direction: i8) {
    match setting {
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

fn draw_header(frame: &mut Frame<'_>, area: Rect, theme: &OmarchyTheme) {
    let title = Line::from(vec![
        Span::styled(
            " DESKLINK ",
            Style::default()
                .fg(color(theme.background))
                .bg(color(theme.accent))
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            "  Omarchy remote desktop",
            Style::default().fg(color(theme.foreground)),
        ),
    ]);

    frame.render_widget(
        Paragraph::new(title)
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
                .block(titled_panel(" Devices ", theme)),
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
        .block(titled_panel(" Devices ", theme))
        .highlight_symbol("  ▸ ")
        .highlight_style(
            Style::default()
                .fg(color(theme.foreground))
                .bg(color(theme.selection))
                .add_modifier(Modifier::BOLD),
        );

    frame.render_stateful_widget(list, area, &mut state.list);
}

fn device_item(node: &DiscoveredNode, theme: &OmarchyTheme) -> ListItem<'static> {
    let name = if node.is_local {
        format!("{} (This device)", node.name)
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
        Line::from(vec![
            Span::styled("  ● ", status_style(node, theme)),
            Span::styled(name, Style::default().fg(color(foreground))),
        ]),
        Line::from(vec![
            Span::raw("    "),
            Span::styled(
                format!("{:<13}", status_label(node.status)),
                status_style(node, theme),
            ),
            Span::styled(
                format!("{:<10}", connection_label(node.connection)),
                Style::default().fg(color(theme.muted)),
            ),
            Span::styled(latency, Style::default().fg(color(theme.muted))),
        ]),
        Line::default(),
    ])
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
        |node| detail_lines(node, state, theme, inner_width),
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

    let paragraph = Paragraph::new(lines).block(titled_panel(" Overview ", theme));
    if state.detail_expanded {
        frame.render_widget(paragraph.wrap(ratatui::widgets::Wrap { trim: false }), area);
    } else {
        frame.render_widget(paragraph, area);
    }
}

fn detail_lines(
    node: &DiscoveredNode,
    state: &AppState,
    theme: &OmarchyTheme,
    width: usize,
) -> Vec<Line<'static>> {
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
            .fg(color(if node.is_local {
                theme.muted
            } else {
                theme.accent
            }))
            .add_modifier(Modifier::BOLD),
    ));
    lines.extend(styled_wrapped_lines(
        &node.address.to_string(),
        width,
        Style::default().fg(color(theme.muted)),
    ));
    lines.push(Line::default());
    lines.extend(wrapped_property_lines(
        "Status",
        status_label(node.status),
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

    if let Some(reason) = state.unavailable_reason() {
        lines.push(Line::default());
        lines.extend(styled_wrapped_lines(
            reason,
            width,
            Style::default().fg(color(theme.muted)),
        ));
    }

    if let Some((kind, message)) = state.message() {
        lines.push(Line::default());
        let (title, accent) = match kind {
            MessageKind::Error => ("ERROR", theme.error),
            MessageKind::Notice => ("MESSAGE", theme.success),
        };
        lines.extend(styled_wrapped_lines(
            title,
            width,
            Style::default()
                .fg(color(accent))
                .add_modifier(Modifier::BOLD),
        ));
        lines.extend(styled_wrapped_lines(
            message,
            width,
            Style::default().fg(color(accent)),
        ));
    }

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
        label(" navigate   ", theme),
        key("r", theme),
        label(" refresh   ", theme),
    ];

    if state.selected_remote().is_some() {
        spans.extend([key("Enter", theme), label(" connect   ", theme)]);
    }

    spans.extend([
        key("s", theme),
        label(" settings   ", theme),
        key("q", theme),
        label(" quit", theme),
    ]);

    frame.render_widget(
        Paragraph::new(Line::from(spans))
            .alignment(Alignment::Center)
            .block(panel(theme)),
        area,
    );
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
                "Discovering DeskLink devices…",
                Style::default().fg(color(theme.accent)),
            )),
        ];
    }

    if let Some(error) = &state.error {
        return vec![
            Line::default(),
            Line::from(Span::styled(
                "Discovery failed",
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
            "No compatible devices found",
            Style::default().fg(color(theme.foreground)),
        )),
        Line::from(Span::styled(
            "Start omdesk-agent on another Omarchy device",
            Style::default().fg(color(theme.muted)),
        )),
    ]
}

fn status_label(status: NodeStatus) -> &'static str {
    match status {
        NodeStatus::Ready => "READY",
        NodeStatus::Offline => "OFFLINE",
        NodeStatus::AgentUnknown => "UNKNOWN",
        NodeStatus::Incompatible => "INCOMPATIBLE",
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
        match node.status {
            NodeStatus::Ready => theme.success,
            NodeStatus::Offline | NodeStatus::Incompatible => theme.error,
            NodeStatus::AgentUnknown => theme.warning,
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
    use omdesk_core::NodeCapabilities;
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

        let lines = detail_lines(state.selected().expect("selected node"), &state, &theme, 18);

        assert!(lines.len() > 12);
        assert!(lines.iter().all(|line| line.width() <= 18));
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
    fn test_loading_state_is_visible_before_discovery_finishes() {
        let mut state = state();
        state.loading = true;
        let theme = state.theme.current().clone();
        let lines = empty_state_lines(&state, &theme);

        assert!(lines[1].to_string().contains("Discovering"));
    }

    #[test]
    fn test_completed_activity_requires_terminal_reset() {
        let message = AsyncMessage::Activity(Ok("disconnected".to_owned()));

        assert!(requires_terminal_reset(&message));
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
    fn test_local_device_is_visible_but_cannot_connect() {
        let mut local = node("omarchy");
        local.is_local = true;
        let mut state = state();

        apply_refresh(&mut state, Ok(vec![local]));

        assert!(state.selected().is_some());
        assert!(state.selected_remote().is_none());
        assert_eq!(
            state.unavailable_reason(),
            Some("This device cannot connect to itself")
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
