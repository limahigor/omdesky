use async_trait::async_trait;
use omdesky_core::{
    ControlCapability, Display, DisplayId, InputMode, MeshPeer, NodeId, RemoteCommand,
    RemoteDesktopTopology, SessionRole, StreamProfile, Window, WindowSelector, Workspace,
    WorkspaceTarget,
};
use omdesky_protocol::{
    ActiveWindowResponse, CommandRequest, CommandResponse, HealthResponse, NodeInfoResponse,
    SunshinePairChallengeResponse, SunshinePairRequest, SunshineStatusResponse,
};
use std::{collections::BTreeMap, net::IpAddr, path::PathBuf, time::Duration};
use time::OffsetDateTime;

pub type PortResult<T> = Result<T, PortError>;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PortError {
    pub code: &'static str,
    pub message: String,
    pub retryable: bool,
}

impl PortError {
    pub fn new(code: &'static str, message: impl Into<String>, retryable: bool) -> Self {
        let message = message.into();

        tracing::debug!(code, retryable, detail = %message, "operation.failed");

        Self {
            code,
            message,
            retryable,
        }
    }

    pub fn user_message(&self) -> &str {
        match self.code {
            "AGENT_UNREACHABLE" => {
                "This device could not be reached. Check that it is online and connected to Tailscale."
            }
            "AGENT_PROTOCOL_INVALID" => {
                "This device sent an unexpected response. Make sure Omdesky is up to date on both devices."
            }
            "AGENT_REQUEST_FAILED" => {
                "The other device could not complete the request. Please try again."
            }
            "UNAUTHORIZED" => {
                "This device is not allowed to connect. Add it to the allowed controllers first."
            }
            "HYPRLAND_UNAVAILABLE" | "HYPRLAND_DISPATCH_FAILED" => {
                "The desktop could not be controlled. Make sure Hyprland is running."
            }
            "HYPRLAND_TARGET_MISSING" | "WINDOW_NOT_FOUND" => {
                "The requested window is no longer available."
            }
            "WORKSPACE_NOT_FOUND" => "The requested workspace could not be found.",
            "DISPLAY_NOT_FOUND" | "DISPLAY_SWITCH_FAILED" => {
                "The requested display is not available for streaming."
            }
            "SUNSHINE_NOT_INSTALLED" => {
                "Sunshine is not installed on the device you are trying to connect to."
            }
            "SUNSHINE_NOT_RUNNING" => {
                "Sunshine is not running on the device you are trying to connect to."
            }
            "SUNSHINE_NOT_READY" => {
                "The other device is not ready to stream yet. Check Sunshine and try again."
            }
            "SUNSHINE_API_UNAVAILABLE" => {
                "Sunshine needs to be configured on the other device before pairing."
            }
            "SUNSHINE_PAIRING_FAILED" | "MOONLIGHT_PAIRING_FAILED" => {
                "The devices could not be paired. Check Sunshine and try again."
            }
            "MOONLIGHT_PAIRING_REQUIRED" => {
                "This device is not paired yet. Run `omdesky pair` first."
            }
            "MOONLIGHT_NOT_INSTALLED" => "Moonlight is not installed on this device.",
            "STREAM_START_FAILED" => {
                "The stream could not be started. Check Moonlight and try again."
            }
            "LOCAL_AGENT_UNAVAILABLE" => {
                "The local Omdesky agent is not running. Start omdesky-agent and try again."
            }
            "REMOTE_AGENT_UNAVAILABLE" => {
                "The remote Omdesky agent stopped responding. The stream was closed."
            }
            "VERSION_INCOMPATIBLE" => {
                "The other device runs a different Omdesky release. Install the same version on both computers."
            }
            "LOCAL_AGENT_INCOMPATIBLE" => {
                "The local Omdesky agent runs a different release. Restart omdesky-agent after upgrading."
            }
            "CAPABILITY_DENIED" => {
                "The other device does not allow this action. Grant it there with `omdesky access allow`."
            }
            "CALLBACK_ACCESS_MISSING" => {
                "This computer does not let the other device send shortcuts back. Run `omdesky access allow` here, naming the other device."
            }
            "CONTROLLER_UNREACHABLE" => {
                "The other device could not reach this computer's agent. Check that omdesky-agent is running here."
            }
            "PEER_IDENTITY_UNKNOWN" => {
                "The other device could not be identified through Tailscale."
            }
            "LOCAL_SUNSHINE_UNCONFIGURED" => {
                "Sunshine is not configured on this device. Run `omdesky setup` and try again."
            }
            "ACCESS_STORE_FAILED" => {
                "The allowed devices list could not be updated. Check its file permissions."
            }
            "OMARCHY_UNSUPPORTED_VERSION" => {
                "This Omarchy version is not supported. Omarchy 4 is required."
            }
            "OMARCHY_VERSION_UNKNOWN" => "The installed Omarchy version could not be detected.",
            "COMMAND_TIMED_OUT" => "The operation took too long. Please try again.",
            "COMMAND_NOT_AVAILABLE" => {
                "A required program is not installed or could not be started."
            }
            "INVALID_COMMAND" | "COMMAND_NOT_EXECUTABLE" | "INVALID_SESSION_TRANSITION" => {
                "This action is not available right now."
            }
            "LAUNCHER_IO_FAILED" => {
                "The desktop shortcut could not be updated. Check the file permissions."
            }
            _ => "Omdesky could not complete the operation. Please try again.",
        }
    }
}

impl std::fmt::Display for PortError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.user_message())
    }
}

impl std::error::Error for PortError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MeshNodeIdentity {
    pub tailnet_node_id: String,
    pub user: Option<String>,
    pub hostname: Option<String>,
    pub addresses: Vec<IpAddr>,
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

pub const MAX_STREAM_APPLICATION_BYTES: usize = 64;

impl StreamHostDescriptor {
    pub fn validate(&self) -> PortResult<()> {
        let name = self.application.as_str();
        let accepted = !name.is_empty()
            && name.len() <= MAX_STREAM_APPLICATION_BYTES
            && !name.starts_with('-')
            && name.chars().all(|character| {
                character.is_ascii_alphanumeric() || matches!(character, ' ' | '.' | '_' | '-')
            });

        if accepted {
            Ok(())
        } else {
            Err(PortError::new(
                "INVALID_STREAM_APPLICATION",
                "the remote node advertised an unusable streaming application name",
                false,
            ))
        }
    }
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
    #[serde(default)]
    pub capabilities: Vec<ControlCapability>,
}

impl AllowedController {
    pub fn new(
        tailnet_node_id: impl Into<String>,
        label: Option<String>,
        added_at: OffsetDateTime,
        capabilities: impl IntoIterator<Item = ControlCapability>,
    ) -> Self {
        let mut capabilities: Vec<_> = capabilities.into_iter().collect();
        capabilities.sort();
        capabilities.dedup();

        Self {
            tailnet_node_id: tailnet_node_id.into(),
            label,
            added_at,
            capabilities,
        }
    }

    pub fn allows(&self, capability: ControlCapability) -> bool {
        self.capabilities.contains(&capability)
    }
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
    pub stdout_limit: usize,
    pub stderr_limit: usize,
    pub environment_policy: EnvironmentPolicy,
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
            stdout_limit: 1024 * 1024,
            stderr_limit: 64 * 1024,
            environment_policy: EnvironmentPolicy::Session,
            stdin: StdinPolicy::Null,
            capture: CapturePolicy::Both,
            redacted_arg_indexes: Vec::new(),
        }
    }

    pub fn with_environment_policy(mut self, policy: EnvironmentPolicy) -> Self {
        self.environment_policy = policy;

        self
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum EnvironmentPolicy {
    #[default]
    Session,
    Inherited,
    Empty,
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

    async fn identify_source(&self, source: IpAddr) -> PortResult<Option<MeshNodeIdentity>>;
}

#[async_trait]
pub trait AgentClient: Send + Sync {
    async fn health(&self, endpoint: &AgentEndpoint) -> PortResult<HealthResponse>;
    async fn node_info(&self, endpoint: &AgentEndpoint) -> PortResult<NodeInfoResponse>;
    async fn granted_capabilities(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<Vec<ControlCapability>>;
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
    async fn sunshine_pair_challenge(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<SunshinePairChallengeResponse>;
    async fn sunshine_pair(
        &self,
        endpoint: &AgentEndpoint,
        request: SunshinePairRequest,
    ) -> PortResult<()>;

    async fn send_command(
        &self,
        endpoint: &AgentEndpoint,
        request: CommandRequest,
    ) -> PortResult<CommandResponse>;
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
    pub window: WindowSelector,
}

#[async_trait]
pub trait SessionKeybindInstaller: Send + Sync {
    async fn install(&self, config: SessionKeybindConfig) -> PortResult<()>;
    async fn clear(&self) -> PortResult<()>;
}

#[async_trait]
pub trait StreamWindowLocator: Send + Sync {
    async fn window_for_process(&self, pid: u32) -> PortResult<Option<WindowSelector>>;
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
pub trait StreamDisplayController: Send + Sync {
    async fn current_display(&self) -> PortResult<DisplayId>;
    async fn switch_display(&self, display: &DisplayId) -> PortResult<()>;
}

#[async_trait]
pub trait DisplayTopologySource: Send + Sync {
    async fn topology(&self) -> PortResult<RemoteDesktopTopology>;
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
}

#[async_trait]
pub trait LauncherStore: Send + Sync {
    async fn create(&self, launcher: LauncherSpec) -> PortResult<PathBuf>;
    async fn list(&self) -> PortResult<Vec<PathBuf>>;
    async fn remove(&self, node_id: NodeId) -> PortResult<()>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_port_error_displays_friendly_message_without_internal_details() {
        let error = PortError::new(
            "AGENT_UNREACHABLE",
            "error sending request for url (http://100.64.0.7:48155/v1/node)",
            true,
        );

        assert_eq!(
            error.to_string(),
            "This device could not be reached. Check that it is online and connected to Tailscale."
        );
        assert!(!error.to_string().contains("100.64.0.7"));
        assert_eq!(
            error.message,
            "error sending request for url (http://100.64.0.7:48155/v1/node)"
        );
    }

    #[test]
    fn test_unknown_port_error_has_safe_fallback() {
        let error = PortError::new("PRIVATE_BACKEND_FAILURE", "secret backend detail", false);

        assert_eq!(
            error.to_string(),
            "Omdesky could not complete the operation. Please try again."
        );
        assert!(!error.to_string().contains("PRIVATE_BACKEND_FAILURE"));
        assert!(!error.to_string().contains("secret backend detail"));
    }
}
