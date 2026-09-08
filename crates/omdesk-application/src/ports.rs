use async_trait::async_trait;
use omdesk_core::{
    ConnectionKind, Display, InputMode, MeshPeer, NodeId, RemoteCommand, SessionRole,
    StreamProfile, Window, Workspace, WorkspaceTarget,
};
use omdesk_protocol::{
    ActiveWindowResponse, HealthResponse, NodeInfoResponse, SunshinePairRequest,
    SunshineStatusResponse,
};
use std::{collections::BTreeMap, net::IpAddr, path::PathBuf, time::Duration};
use time::OffsetDateTime;

pub type PortResult<T> = Result<T, PortError>;

#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{code}: {message}")]
pub struct PortError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl PortError {
    pub fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code,
            message: message.into(),
            retryable,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshNodeIdentity {
    pub tailnet_node_id: String,
    pub user: Option<String>,
    pub hostname: Option<String>,
    pub addresses: Vec<IpAddr>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConnectionInfo {
    pub kind: ConnectionKind,
    pub latency_ms: Option<u32>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AgentEndpoint {
    pub address: IpAddr,
    pub port: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamHostDescriptor {
    pub address: IpAddr,
    pub application: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PairingState {
    Paired,
    Required,
    Unsupported,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PendingPairing {
    pub pin: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamLaunchRequest {
    pub host: StreamHostDescriptor,
    pub profile: StreamProfile,
    pub fullscreen: bool,
    pub input_mode: InputMode,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostReadiness {
    pub installed: bool,
    pub running: bool,
    pub capture_ready: bool,
    pub input_ready: bool,
    pub exposure_warning: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Notification {
    pub summary: String,
    pub body: String,
}

#[derive(Clone, Debug, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
pub struct AllowedController {
    pub tailnet_node_id: String,
    pub label: Option<String>,
    pub added_at: OffsetDateTime,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LauncherSpec {
    pub node_id: NodeId,
    pub display_name: String,
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandSpec {
    pub program: String,
    pub args: Vec<String>,
    pub environment: BTreeMap<String, String>,
    pub working_directory: Option<PathBuf>,
    pub timeout: Duration,
    pub stdin: StdinPolicy,
    pub capture: CapturePolicy,
    pub redacted_arg_indexes: Vec<usize>,
}

impl CommandSpec {
    pub fn new(program: impl Into<String>, args: impl IntoIterator<Item = String>) -> Self {
        Self {
            program: program.into(),
            args: args.into_iter().collect(),
            environment: BTreeMap::new(),
            working_directory: None,
            timeout: Duration::from_secs(10),
            stdin: StdinPolicy::Null,
            capture: CapturePolicy::Both,
            redacted_arg_indexes: Vec::new(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StdinPolicy {
    Null,
    Inherit,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapturePolicy {
    Both,
    Inherit,
    Discard,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[async_trait]
pub trait ChildProcess: Send {
    fn id(&self) -> Option<u32>;
    async fn wait(&mut self) -> PortResult<i32>;
    async fn terminate(&mut self) -> PortResult<()>;
}

#[async_trait]
pub trait CommandRunner: Send + Sync {
    async fn run(&self, spec: CommandSpec) -> PortResult<CommandOutput>;
    async fn spawn(&self, spec: CommandSpec) -> PortResult<Box<dyn ChildProcess>>;
}

#[async_trait]
pub trait MeshNetwork: Send + Sync {
    async fn local_node(&self) -> PortResult<MeshNodeIdentity>;
    async fn peers(&self) -> PortResult<Vec<MeshPeer>>;
    async fn connection_info(&self, tailnet_node_id: &str) -> PortResult<ConnectionInfo>;

    async fn identify_source(&self, source: IpAddr) -> PortResult<Option<MeshNodeIdentity>>;
}

#[async_trait]
pub trait AgentClient: Send + Sync {
    async fn health(&self, endpoint: &AgentEndpoint) -> PortResult<HealthResponse>;
    async fn node_info(&self, endpoint: &AgentEndpoint) -> PortResult<NodeInfoResponse>;
    async fn displays(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Display>>;
    async fn workspaces(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Workspace>>;
    async fn windows(&self, endpoint: &AgentEndpoint) -> PortResult<Vec<Window>>;
    async fn active_window(&self, endpoint: &AgentEndpoint) -> PortResult<ActiveWindowResponse>;
    async fn focus_workspace(
        &self,
        endpoint: &AgentEndpoint,
        target: WorkspaceTarget,
    ) -> PortResult<()>;
    async fn focus_window(&self, endpoint: &AgentEndpoint, window: &str) -> PortResult<()>;
    async fn sunshine_status(&self, endpoint: &AgentEndpoint)
    -> PortResult<SunshineStatusResponse>;
    async fn sunshine_pair(
        &self,
        endpoint: &AgentEndpoint,
        request: SunshinePairRequest,
    ) -> PortResult<()>;

    async fn send_command(
        &self,
        endpoint: &AgentEndpoint,
        command: RemoteCommand,
    ) -> PortResult<()>;
}

#[async_trait]
pub trait RemoteOmarchy: Send + Sync {
    async fn displays(&self) -> PortResult<Vec<Display>>;
    async fn workspaces(&self) -> PortResult<Vec<Workspace>>;
    async fn windows(&self) -> PortResult<Vec<Window>>;
    async fn active_window(&self) -> PortResult<Option<Window>>;
    async fn focus_workspace(&self, target: WorkspaceTarget) -> PortResult<()>;
    async fn focus_window(&self, id: &str) -> PortResult<()>;
}

#[async_trait]
pub trait CommandExecutor: Send + Sync {
    async fn execute(&self, command: RemoteCommand) -> PortResult<()>;
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionKeybindConfig {
    pub role: SessionRole,
    pub controller: Option<AgentEndpoint>,
}

#[async_trait]
pub trait SessionKeybindInstaller: Send + Sync {
    async fn install(&self, config: SessionKeybindConfig) -> PortResult<()>;
    async fn clear(&self) -> PortResult<()>;
}

#[async_trait]
pub trait StreamClient: Send + Sync {
    async fn pairing_state(&self, host: &StreamHostDescriptor) -> PortResult<PairingState>;
    async fn begin_pairing(&self, host: &StreamHostDescriptor) -> PortResult<PendingPairing>;
    async fn launch(&self, request: StreamLaunchRequest) -> PortResult<Box<dyn ChildProcess>>;
}

#[async_trait]
pub trait StreamHost: Send + Sync {
    async fn readiness(&self) -> PortResult<HostReadiness>;
    async fn status(&self) -> PortResult<SunshineStatusResponse>;
    async fn displays(&self) -> PortResult<Vec<Display>>;
    async fn submit_pairing_pin(&self, request: SunshinePairRequest) -> PortResult<()>;
}

#[async_trait]
pub trait DesktopEnvironment: Send + Sync {
    async fn active_display(&self) -> PortResult<Option<Display>>;
    async fn set_input_mode(&self, mode: InputMode) -> PortResult<()>;
}

#[async_trait]
pub trait NotificationService: Send + Sync {
    async fn send(&self, notification: Notification) -> PortResult<()>;
}

#[async_trait]
pub trait AccessStore: Send + Sync {
    async fn list(&self) -> PortResult<Vec<AllowedController>>;
    async fn allow(&self, controller: AllowedController) -> PortResult<()>;
    async fn revoke(&self, tailnet_node_id: &str) -> PortResult<()>;
    async fn is_allowed(&self, tailnet_node_id: &str) -> PortResult<bool>;
}

pub trait Clock: Send + Sync {
    fn now(&self) -> OffsetDateTime;
}

#[async_trait]
pub trait LauncherStore: Send + Sync {
    async fn create(&self, launcher: LauncherSpec) -> PortResult<PathBuf>;
    async fn list(&self) -> PortResult<Vec<PathBuf>>;
    async fn remove(&self, node_id: NodeId) -> PortResult<()>;
}
