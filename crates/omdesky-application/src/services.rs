use crate::access::CallbackAccess;
use crate::ports::{
    AgentClient, AgentEndpoint, ChildProcess, Notification, NotificationService, PairingState,
    PortError, PortResult, SessionKeybindConfig, SessionKeybindInstaller, StreamClient,
    StreamHostDescriptor, StreamLaunchRequest, StreamWindowLocator,
};
use omdesky_core::{
    InputMode, RemoteCommand, SessionEndpoint, SessionGrant, SessionRole, StreamProfile,
    WindowSelector, WorkspaceTarget,
};
use omdesky_protocol::{CommandRequest, SunshinePairRequest};
use std::{future::Future, sync::Arc};

pub const MOONLIGHT_WINDOW_CLASS: &str = "com.moonlight_stream.Moonlight";

pub struct PairStream {
    agent: Arc<dyn AgentClient>,
    stream: Arc<dyn StreamClient>,
    client_name: String,
}

impl PairStream {
    pub fn new(
        agent: Arc<dyn AgentClient>,
        stream: Arc<dyn StreamClient>,
        client_name: String,
    ) -> Self {
        Self {
            agent,
            stream,
            client_name,
        }
    }

    pub async fn execute(&self, endpoint: &AgentEndpoint) -> PortResult<()> {
        let host = StreamHostDescriptor {
            address: endpoint.address,
            application: "Desktop".to_owned(),
        };

        if matches!(
            self.stream.pairing_state(&host).await?,
            PairingState::Paired
        ) {
            return Ok(());
        }

        let status = self.agent.sunshine_status(endpoint).await?;

        if !status.running {
            return Err(PortError::new(
                "SUNSHINE_NOT_RUNNING",
                "Sunshine is not running on the remote node",
                true,
            ));
        }

        if !status.pairing_api_available {
            return Err(PortError::new(
                "SUNSHINE_API_UNAVAILABLE",
                "Sunshine admin credentials are missing on the remote host. Configure them on that machine (Settings -> Configure Sunshine credentials, or `omdesky setup`), not on this controller.",
                false,
            ));
        }

        let challenge = self.agent.sunshine_pair_challenge(endpoint).await?;
        let pending = self.stream.begin_pairing(&host).await?;

        self.agent
            .sunshine_pair(
                endpoint,
                SunshinePairRequest {
                    pairing_id: challenge.pairing_id,
                    pin: pending.pin().to_owned(),
                    client_name: self.client_name.clone(),
                },
            )
            .await?;

        pending.complete().await?;

        match self.stream.pairing_state(&host).await? {
            PairingState::Paired => Ok(()),
            _ => Err(PortError::new(
                "MOONLIGHT_PAIRING_FAILED",
                "Moonlight did not report the host as paired",
                true,
            )),
        }
    }
}

pub struct ConnectRequest {
    pub endpoint: AgentEndpoint,
    pub controller_endpoint: AgentEndpoint,
    pub profile: StreamProfile,
    pub fullscreen: bool,
    pub input_mode: InputMode,
    pub focus_workspace: Option<WorkspaceTarget>,
    pub focus_window: Option<String>,
    pub auto_pair: bool,
}

pub const STREAM_WINDOW_LOOKUP_ATTEMPTS: u32 = 20;
pub const STREAM_WINDOW_LOOKUP_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(250);

pub struct ConnectNode {
    agent: Arc<dyn AgentClient>,
    callback_access: CallbackAccess,
    stream: Arc<dyn StreamClient>,
    keybinds: Arc<dyn SessionKeybindInstaller>,
    notifications: Arc<dyn NotificationService>,
    windows: Option<Arc<dyn StreamWindowLocator>>,
    pairing: PairStream,
}

impl ConnectNode {
    pub fn new(
        agent: Arc<dyn AgentClient>,
        callback_access: CallbackAccess,
        stream: Arc<dyn StreamClient>,
        keybinds: Arc<dyn SessionKeybindInstaller>,
        notifications: Arc<dyn NotificationService>,
        client_name: String,
    ) -> Self {
        let pairing = PairStream::new(agent.clone(), stream.clone(), client_name);
        Self {
            agent,
            callback_access,
            stream,
            keybinds,
            notifications,
            windows: None,
            pairing,
        }
    }

    pub fn with_window_locator(mut self, windows: Arc<dyn StreamWindowLocator>) -> Self {
        self.windows = Some(windows);

        self
    }

    async fn locate_stream_window(&self, pid: Option<u32>) -> WindowSelector {
        let fallback = WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned());

        let (Some(windows), Some(pid)) = (self.windows.as_ref(), pid) else {
            return fallback;
        };

        for _ in 0..STREAM_WINDOW_LOOKUP_ATTEMPTS {
            match windows.window_for_process(pid).await {
                Ok(Some(window)) => return window,
                Ok(None) => {}
                Err(error) => {
                    tracing::debug!(
                        code = error.code,
                        detail = %error.message,
                        "session.window_lookup_failed"
                    );

                    return fallback;
                }
            }

            tokio::time::sleep(STREAM_WINDOW_LOOKUP_INTERVAL).await;
        }

        tracing::warn!("session.window_lookup_timed_out");

        fallback
    }

    pub async fn displays(
        &self,
        endpoint: &AgentEndpoint,
    ) -> PortResult<Vec<omdesky_core::Display>> {
        self.agent.displays(endpoint).await
    }

    pub async fn execute(&self, request: ConnectRequest) -> PortResult<i32> {
        self.execute_with_started(request, |_| async {}).await
    }

    pub async fn execute_with_started<F, Fut>(
        &self,
        request: ConnectRequest,
        on_started: F,
    ) -> PortResult<i32>
    where
        F: FnOnce(Option<u32>) -> Fut,
        Fut: std::future::Future<Output = ()>,
    {
        self.callback_access
            .verify(request.endpoint.address)
            .await?;

        let status = self.agent.sunshine_status(&request.endpoint).await?;

        if !status.ready {
            return Err(PortError::new(
                "SUNSHINE_NOT_READY",
                "Sunshine is not ready to stream on the remote node",
                true,
            ));
        }

        let host = StreamHostDescriptor {
            address: request.endpoint.address,
            application: status.desktop_app.unwrap_or_else(|| "Desktop".to_owned()),
        };

        if !matches!(
            self.stream.pairing_state(&host).await?,
            PairingState::Paired
        ) {
            if request.auto_pair {
                self.pairing.execute(&request.endpoint).await?;
            } else {
                return Err(PortError::new(
                    "MOONLIGHT_PAIRING_REQUIRED",
                    "Moonlight is not paired with the host; run `omdesky pair` first",
                    false,
                ));
            }
        }

        if let Some(target) = request.focus_workspace.clone() {
            self.agent
                .focus_workspace(&request.endpoint, target)
                .await?;
        }

        if let Some(window) = &request.focus_window {
            self.agent.focus_window(&request.endpoint, window).await?;
        }

        let stream_request = StreamLaunchRequest {
            host,
            profile: request.profile.clone(),
            fullscreen: request.fullscreen,
            input_mode: request.input_mode,
        };

        verify_agents(
            self.agent.as_ref(),
            &request.controller_endpoint,
            &request.endpoint,
        )
        .await?;
        let process = self.stream.launch(stream_request).await?;
        let moonlight_pid = process.id();

        let mut grant = None;
        let mut heartbeat = None;

        if request.input_mode == InputMode::Remote {
            let window = self.locate_stream_window(moonlight_pid).await;

            match self.attach_session(&request, window).await {
                Ok(session) => {
                    heartbeat = Some(SessionHeartbeat::spawn(
                        self.agent.clone(),
                        request.endpoint.clone(),
                        session,
                    ));
                    grant = Some(session);
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: "Remote shortcuts are ready. Press SUPER+R to switch shortcut capture.".to_owned(),
                        })
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        code = error.code,
                        detail = %error.message,
                        "session.shortcuts_setup_failed"
                    );
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: format!(
                                "The stream started, but remote shortcuts are unavailable. {}",
                                error.user_message()
                            ),
                        })
                        .await;
                }
            }
        }

        on_started(moonlight_pid).await;

        let exit = wait_for_process_or_monitor(
            process,
            monitor_agents(
                self.agent.clone(),
                request.controller_endpoint.clone(),
                request.endpoint.clone(),
            ),
        )
        .await;

        drop(heartbeat);

        if let Some(grant) = grant {
            match self.detach_session(&request, grant).await {
                Ok(()) => {
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: "The stream ended and your local shortcuts were restored."
                                .to_owned(),
                        })
                        .await;
                }
                Err(error) => {
                    tracing::warn!(
                        code = error.code,
                        detail = %error.message,
                        "session.shortcuts_restore_failed"
                    );
                    let _ = self
                        .notifications
                        .send(Notification {
                            summary: "Omdesky".to_owned(),
                            body: "The stream ended, but local shortcuts could not be restored. Restart Hyprland before reconnecting.".to_owned(),
                        })
                        .await;
                }
            }
        }

        exit
    }

    async fn attach_session(
        &self,
        request: &ConnectRequest,
        window: WindowSelector,
    ) -> PortResult<SessionGrant> {
        self.keybinds
            .install(SessionKeybindConfig {
                role: SessionRole::Controller,
                controller: None,
                window: window.clone(),
            })
            .await?;

        let controller = SessionEndpoint {
            address: request.controller_endpoint.address,
            port: request.controller_endpoint.port,
        };

        let attach = self
            .agent
            .send_command(
                &request.endpoint,
                CommandRequest::new(RemoteCommand::AttachSession {
                    role: SessionRole::Remote,
                    controller: Some(controller),
                    window: Some(window),
                }),
            )
            .await;

        match attach {
            Ok(response) => response.session.ok_or_else(|| {
                PortError::new(
                    "SESSION_NOT_GRANTED",
                    "the remote agent accepted the session without issuing a lease",
                    false,
                )
            }),
            Err(error) => {
                let _ = self.keybinds.clear().await;
                Err(error)
            }
        }
    }

    async fn detach_session(
        &self,
        request: &ConnectRequest,
        grant: SessionGrant,
    ) -> PortResult<()> {
        let local = self.keybinds.clear().await;
        let remote = self
            .agent
            .send_command(
                &request.endpoint,
                CommandRequest::for_session(RemoteCommand::DetachSession, grant.claim()),
            )
            .await;

        local.and(remote.map(|_| ()))
    }
}

const AGENT_HEALTH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(3);
const AGENT_HEALTH_FAILURE_LIMIT: u8 = 2;

const MIN_RENEWAL_INTERVAL: std::time::Duration = std::time::Duration::from_secs(2);

pub struct SessionHeartbeat {
    handle: tokio::task::JoinHandle<()>,
}

impl SessionHeartbeat {
    fn spawn(agent: Arc<dyn AgentClient>, endpoint: AgentEndpoint, grant: SessionGrant) -> Self {
        let interval = renewal_interval(grant.lease_seconds);

        Self {
            handle: tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.tick().await;

                loop {
                    ticker.tick().await;

                    let renewal = agent
                        .send_command(
                            &endpoint,
                            CommandRequest::for_session(RemoteCommand::RenewSession, grant.claim()),
                        )
                        .await;

                    if let Err(error) = renewal {
                        tracing::debug!(
                            code = error.code,
                            detail = %error.message,
                            "session.renewal_failed"
                        );
                    }
                }
            }),
        }
    }
}

impl Drop for SessionHeartbeat {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn renewal_interval(lease_seconds: u64) -> std::time::Duration {
    std::time::Duration::from_secs(lease_seconds.max(1) / 3).max(MIN_RENEWAL_INTERVAL)
}

fn agent_health_failure(
    local_failures: u8,
    remote_failures: u8,
    failure_limit: u8,
) -> Option<PortError> {
    if local_failures >= failure_limit {
        Some(PortError::new(
            "LOCAL_AGENT_UNAVAILABLE",
            "the local omdesky-agent stopped responding",
            true,
        ))
    } else if remote_failures >= failure_limit {
        Some(PortError::new(
            "REMOTE_AGENT_UNAVAILABLE",
            "the remote omdesky-agent stopped responding",
            true,
        ))
    } else {
        None
    }
}

async fn verify_agents(
    agent: &dyn AgentClient,
    local: &AgentEndpoint,
    remote: &AgentEndpoint,
) -> PortResult<()> {
    let (local_result, remote_result) = tokio::join!(agent.health(local), agent.health(remote));

    match agent_health_failure(
        u8::from(local_result.is_err()),
        u8::from(remote_result.is_err()),
        1,
    ) {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

async fn monitor_agents(
    agent: Arc<dyn AgentClient>,
    local: AgentEndpoint,
    remote: AgentEndpoint,
) -> PortError {
    let mut interval = tokio::time::interval(AGENT_HEALTH_INTERVAL);
    let mut local_failures = 0;
    let mut remote_failures = 0;
    interval.tick().await;

    loop {
        interval.tick().await;
        let (local_result, remote_result) =
            tokio::join!(agent.health(&local), agent.health(&remote));
        local_failures = if local_result.is_ok() {
            0
        } else {
            local_failures + 1
        };
        remote_failures = if remote_result.is_ok() {
            0
        } else {
            remote_failures + 1
        };

        if let Some(error) =
            agent_health_failure(local_failures, remote_failures, AGENT_HEALTH_FAILURE_LIMIT)
        {
            return error;
        }
    }
}

pub async fn wait_for_process_or_monitor<F>(
    mut process: Box<dyn ChildProcess>,
    monitor: F,
) -> PortResult<i32>
where
    F: Future<Output = PortError>,
{
    tokio::pin!(monitor);

    tokio::select! {
        exit = process.wait() => exit,
        error = &mut monitor => {
            if let Err(terminate_error) = process.terminate().await {
                tracing::warn!(
                    code = terminate_error.code,
                    detail = %terminate_error.message,
                    "session.stream_terminate_failed"
                );
            }
            Err(error)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct PendingProcess {
        terminated: Arc<std::sync::atomic::AtomicBool>,
    }

    #[async_trait::async_trait]
    impl crate::ports::ChildProcess for PendingProcess {
        fn id(&self) -> Option<u32> {
            Some(42)
        }

        async fn wait(&mut self) -> PortResult<i32> {
            std::future::pending().await
        }

        async fn terminate(&mut self) -> PortResult<()> {
            self.terminated
                .store(true, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_wait_for_process_or_monitor_terminates_stream_on_health_failure() {
        let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let process: Box<dyn crate::ports::ChildProcess> = Box::new(PendingProcess {
            terminated: terminated.clone(),
        });
        let failure = PortError::new("LOCAL_AGENT_UNAVAILABLE", "agent stopped", true);

        let result = wait_for_process_or_monitor(process, async { failure }).await;

        assert_eq!(
            result.expect_err("monitor failure ends session").code,
            "LOCAL_AGENT_UNAVAILABLE"
        );
        assert!(terminated.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[test]
    fn test_agent_health_failure_identifies_local_agent() {
        let error = agent_health_failure(2, 0, 2).expect("local failure");

        assert_eq!(error.code, "LOCAL_AGENT_UNAVAILABLE");
    }

    #[test]
    fn test_agent_health_failure_identifies_remote_agent() {
        let error = agent_health_failure(0, 2, 2).expect("remote failure");

        assert_eq!(error.code, "REMOTE_AGENT_UNAVAILABLE");
    }

    #[test]
    fn test_agent_health_failure_accepts_healthy_agents() {
        assert!(agent_health_failure(0, 0, 2).is_none());
    }

    #[test]
    fn test_agent_health_failure_tolerates_one_transient_failure() {
        assert!(agent_health_failure(1, 1, 2).is_none());
    }
}
