use async_trait::async_trait;
use omdesky_application::ports::{
    AgentEndpoint, PortError, PortResult, SessionKeybindConfig, SessionKeybindInstaller,
};
use omdesky_core::{SessionClaim, SessionGrant, SessionId, SessionRole, WindowSelector};
use std::{sync::Arc, time::Duration};
use tokio::{
    sync::Mutex,
    time::{Instant, interval},
};

pub const DEFAULT_LEASE: Duration = Duration::from_secs(45);
pub const LEASE_SWEEP_INTERVAL: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActiveSession {
    pub id: SessionId,
    pub generation: u64,
    pub owner: String,
    pub role: SessionRole,
    pub controller: Option<AgentEndpoint>,
    pub window: WindowSelector,
}

#[async_trait]
pub trait SessionEffects: Send + Sync {
    async fn install(&self, session: &ActiveSession) -> PortResult<()>;
    async fn clear(&self) -> PortResult<()>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionOutcome {
    Applied,
    Ignored,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionError {
    OwnedByAnotherController,
    NotOwner,
    NotFound,
    EffectsFailed,
}

impl SessionError {
    pub fn code(self) -> &'static str {
        match self {
            SessionError::OwnedByAnotherController => "SESSION_ALREADY_OWNED",
            SessionError::NotOwner => "SESSION_NOT_OWNED",
            SessionError::NotFound => "SESSION_NOT_FOUND",
            SessionError::EffectsFailed => "SESSION_EFFECTS_FAILED",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            SessionError::OwnedByAnotherController => {
                "Another device already owns this desktop session."
            }
            SessionError::NotOwner => "This device does not own the desktop session.",
            SessionError::NotFound => "There is no desktop session to update.",
            SessionError::EffectsFailed => {
                "The desktop session could not be updated. Please try again."
            }
        }
    }
}

struct CoordinatorState {
    active: Option<ActiveSession>,
    expires_at: Option<Instant>,
    generation: u64,
}

pub struct SessionCoordinator {
    state: Mutex<CoordinatorState>,
    effects: Arc<dyn SessionEffects>,
    lease: Duration,
}

impl SessionCoordinator {
    pub fn new(effects: Arc<dyn SessionEffects>, lease: Duration) -> Self {
        Self {
            state: Mutex::new(CoordinatorState {
                active: None,
                expires_at: None,
                generation: 0,
            }),
            effects,
            lease,
        }
    }

    pub fn lease_seconds(&self) -> u64 {
        self.lease.as_secs()
    }

    pub async fn snapshot(&self) -> Option<ActiveSession> {
        self.state.lock().await.active.clone()
    }

    pub async fn reconcile(&self) -> PortResult<()> {
        let mut state = self.state.lock().await;
        state.active = None;
        state.expires_at = None;

        self.effects.clear().await
    }

    pub async fn attach(
        &self,
        owner: &str,
        role: SessionRole,
        controller: Option<AgentEndpoint>,
        window: WindowSelector,
    ) -> Result<SessionGrant, SessionError> {
        let mut state = self.state.lock().await;

        let now = Instant::now();
        let held_by_other = state.active.as_ref().is_some_and(|active| {
            active.owner != owner && state.expires_at.is_some_and(|deadline| deadline > now)
        });

        if held_by_other {
            return Err(SessionError::OwnedByAnotherController);
        }

        state.generation = state.generation.saturating_add(1);
        let candidate = ActiveSession {
            id: SessionId::new(),
            generation: state.generation,
            owner: owner.to_owned(),
            role,
            controller,
            window,
        };

        if let Err(error) = self.effects.clear().await {
            tracing::debug!(
                code = error.code,
                detail = %error.message,
                "session.attach.previous_cleanup_failed"
            );
        }

        if let Err(error) = self.effects.install(&candidate).await {
            tracing::warn!(
                code = error.code,
                detail = %error.message,
                "session.attach.install_failed"
            );

            let _ = self.effects.clear().await;
            state.active = None;
            state.expires_at = None;

            return Err(SessionError::EffectsFailed);
        }

        state.expires_at = Some(now + self.lease);
        state.active = Some(candidate.clone());

        Ok(SessionGrant {
            id: candidate.id,
            generation: candidate.generation,
            lease_seconds: self.lease.as_secs(),
        })
    }

    pub async fn detach(
        &self,
        owner: &str,
        claim: SessionClaim,
    ) -> Result<SessionOutcome, SessionError> {
        let mut state = self.state.lock().await;

        let Some(active) = state.active.clone() else {
            return Ok(SessionOutcome::Ignored);
        };

        if active.owner != owner {
            return Err(SessionError::NotOwner);
        }

        if active.id != claim.id || active.generation != claim.generation {
            tracing::debug!(
                generation = active.generation,
                claimed_generation = claim.generation,
                "session.detach.stale_claim_ignored"
            );

            return Ok(SessionOutcome::Ignored);
        }

        self.effects.clear().await.map_err(|error| {
            tracing::warn!(
                code = error.code,
                detail = %error.message,
                "session.detach.cleanup_failed"
            );

            SessionError::EffectsFailed
        })?;

        state.active = None;
        state.expires_at = None;

        Ok(SessionOutcome::Applied)
    }

    pub async fn renew(
        &self,
        owner: &str,
        claim: SessionClaim,
    ) -> Result<SessionGrant, SessionError> {
        let mut state = self.state.lock().await;

        let Some(active) = state.active.clone() else {
            return Err(SessionError::NotFound);
        };

        if active.owner != owner {
            return Err(SessionError::NotOwner);
        }

        if active.id != claim.id || active.generation != claim.generation {
            return Err(SessionError::NotFound);
        }

        state.expires_at = Some(Instant::now() + self.lease);

        Ok(SessionGrant {
            id: active.id,
            generation: active.generation,
            lease_seconds: self.lease.as_secs(),
        })
    }

    pub async fn expire_due(&self) -> SessionOutcome {
        let mut state = self.state.lock().await;

        let expired = state.active.is_some()
            && state
                .expires_at
                .is_none_or(|deadline| deadline <= Instant::now());

        if !expired {
            return SessionOutcome::Ignored;
        }

        tracing::warn!("session.lease_expired");

        if let Err(error) = self.effects.clear().await {
            tracing::warn!(
                code = error.code,
                detail = %error.message,
                "session.lease_expiry_cleanup_failed"
            );

            return SessionOutcome::Ignored;
        }

        state.active = None;
        state.expires_at = None;

        SessionOutcome::Applied
    }

    pub async fn release(&self) {
        let mut state = self.state.lock().await;

        if state.active.is_none() {
            return;
        }

        if let Err(error) = self.effects.clear().await {
            tracing::warn!(
                code = error.code,
                detail = %error.message,
                "session.shutdown_cleanup_failed"
            );
        }

        state.active = None;
        state.expires_at = None;
    }
}

pub async fn run_lease_expiry(coordinator: Arc<SessionCoordinator>) {
    let mut ticker = interval(LEASE_SWEEP_INTERVAL);
    ticker.tick().await;

    loop {
        ticker.tick().await;
        coordinator.expire_due().await;
    }
}

pub struct KeybindSessionEffects<F> {
    keybinds: Arc<dyn SessionKeybindInstaller>,
    follow_focus: F,
}

impl<F> KeybindSessionEffects<F> {
    pub fn new(keybinds: Arc<dyn SessionKeybindInstaller>, follow_focus: F) -> Self {
        Self {
            keybinds,
            follow_focus,
        }
    }
}

#[async_trait]
impl<F> SessionEffects for KeybindSessionEffects<F>
where
    F: Fn(Option<AgentEndpoint>) + Send + Sync,
{
    async fn install(&self, session: &ActiveSession) -> PortResult<()> {
        self.keybinds
            .install(SessionKeybindConfig {
                role: session.role,
                controller: session.controller.clone(),
                window: session.window.clone(),
            })
            .await?;

        if session.role == SessionRole::Remote {
            (self.follow_focus)(session.controller.clone());
        }

        Ok(())
    }

    async fn clear(&self) -> PortResult<()> {
        (self.follow_focus)(None);

        self.keybinds.clear().await
    }
}

pub fn session_port_error(error: SessionError) -> PortError {
    PortError::new(error.code(), error.message(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        net::{IpAddr, Ipv4Addr},
        sync::{
            Mutex as StdMutex,
            atomic::{AtomicBool, Ordering},
        },
    };

    #[derive(Default)]
    struct RecordingEffects {
        installs: StdMutex<Vec<ActiveSession>>,
        clears: StdMutex<usize>,
        fail_install: AtomicBool,
        fail_clear: AtomicBool,
    }

    impl RecordingEffects {
        fn installs(&self) -> Vec<ActiveSession> {
            self.installs.lock().expect("lock").clone()
        }

        fn clears(&self) -> usize {
            *self.clears.lock().expect("lock")
        }
    }

    #[async_trait]
    impl SessionEffects for RecordingEffects {
        async fn install(&self, session: &ActiveSession) -> PortResult<()> {
            if self.fail_install.load(Ordering::SeqCst) {
                return Err(PortError::new("TEST_FAILURE", "install failed", false));
            }

            self.installs.lock().expect("lock").push(session.clone());

            Ok(())
        }

        async fn clear(&self) -> PortResult<()> {
            if self.fail_clear.load(Ordering::SeqCst) {
                return Err(PortError::new("TEST_FAILURE", "clear failed", false));
            }

            *self.clears.lock().expect("lock") += 1;

            Ok(())
        }
    }

    fn window() -> WindowSelector {
        WindowSelector::Address("0x55aa".to_owned())
    }

    fn endpoint() -> AgentEndpoint {
        AgentEndpoint {
            address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 7)),
            port: 48155,
        }
    }

    fn coordinator(effects: Arc<RecordingEffects>) -> SessionCoordinator {
        SessionCoordinator::new(effects, Duration::from_secs(30))
    }

    #[tokio::test]
    async fn test_attach_grants_an_unpredictable_session_with_a_rising_generation() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = coordinator(effects.clone());

        let first = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("first attach");
        let second = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("same owner may reattach");

        assert_ne!(first.id, second.id);
        assert_eq!(second.generation, first.generation + 1);
        assert_eq!(effects.installs().len(), 2);
    }

    #[tokio::test]
    async fn test_a_second_controller_cannot_take_a_live_session() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = coordinator(effects.clone());

        coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("first attach");

        let error = coordinator
            .attach("node-b", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect_err("the session is owned");

        assert_eq!(error, SessionError::OwnedByAnotherController);
        assert_eq!(
            coordinator
                .snapshot()
                .await
                .expect("session survives")
                .owner,
            "node-a"
        );
    }

    #[tokio::test]
    async fn test_a_stale_detach_cannot_remove_the_current_session() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = coordinator(effects.clone());

        let stale = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("first attach");
        let current = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("second attach");

        let outcome = coordinator
            .detach("node-a", stale.claim())
            .await
            .expect("stale detach is accepted but ignored");

        assert_eq!(outcome, SessionOutcome::Ignored);
        assert_eq!(
            coordinator.snapshot().await.expect("session survives").id,
            current.id
        );
    }

    #[tokio::test]
    async fn test_detach_requires_the_owning_identity() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = coordinator(effects.clone());

        let grant = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("attach");

        let error = coordinator
            .detach("node-b", grant.claim())
            .await
            .expect_err("a foreign identity cannot detach");

        assert_eq!(error, SessionError::NotOwner);
        assert!(coordinator.snapshot().await.is_some());
    }

    #[tokio::test]
    async fn test_a_failed_attach_leaves_no_session_and_clears_its_effects() {
        let effects = Arc::new(RecordingEffects::default());
        effects.fail_install.store(true, Ordering::SeqCst);
        let coordinator = coordinator(effects.clone());

        let error = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect_err("install fails");

        assert_eq!(error, SessionError::EffectsFailed);
        assert!(coordinator.snapshot().await.is_none());
        assert_eq!(effects.clears(), 2);
    }

    #[tokio::test]
    async fn test_a_failed_detach_keeps_the_session_so_cleanup_can_be_retried() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = coordinator(effects.clone());
        let grant = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("attach");

        effects.fail_clear.store(true, Ordering::SeqCst);
        let error = coordinator
            .detach("node-a", grant.claim())
            .await
            .expect_err("cleanup fails");

        assert_eq!(error, SessionError::EffectsFailed);
        assert!(coordinator.snapshot().await.is_some());

        effects.fail_clear.store(false, Ordering::SeqCst);
        assert_eq!(
            coordinator
                .detach("node-a", grant.claim())
                .await
                .expect("retry succeeds"),
            SessionOutcome::Applied
        );
        assert!(coordinator.snapshot().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_an_unrenewed_lease_expires_and_restores_local_input() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = SessionCoordinator::new(effects.clone(), Duration::from_secs(30));
        coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("attach");

        assert_eq!(coordinator.expire_due().await, SessionOutcome::Ignored);

        tokio::time::advance(Duration::from_secs(31)).await;

        assert_eq!(coordinator.expire_due().await, SessionOutcome::Applied);
        assert!(coordinator.snapshot().await.is_none());
    }

    #[tokio::test(start_paused = true)]
    async fn test_renewal_extends_the_lease_for_the_owner_only() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = SessionCoordinator::new(effects, Duration::from_secs(30));
        let grant = coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("attach");

        tokio::time::advance(Duration::from_secs(20)).await;
        let renewed = coordinator
            .renew("node-a", grant.claim())
            .await
            .expect("owner renews");
        assert_eq!(renewed.id, grant.id);

        assert_eq!(
            coordinator
                .renew("node-b", grant.claim())
                .await
                .expect_err("a foreign identity cannot renew"),
            SessionError::NotOwner
        );

        tokio::time::advance(Duration::from_secs(20)).await;
        assert_eq!(coordinator.expire_due().await, SessionOutcome::Ignored);
    }

    #[tokio::test]
    async fn test_an_expired_session_can_be_taken_by_another_controller() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = SessionCoordinator::new(effects, Duration::from_millis(1));
        coordinator
            .attach("node-a", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("attach");

        tokio::time::sleep(Duration::from_millis(5)).await;

        let grant = coordinator
            .attach("node-b", SessionRole::Remote, Some(endpoint()), window())
            .await
            .expect("an expired session may be taken over");

        assert_eq!(
            coordinator.snapshot().await.expect("session").owner,
            "node-b"
        );
        assert_eq!(grant.generation, 2);
    }

    #[tokio::test]
    async fn test_startup_reconciliation_clears_effects_left_by_a_previous_run() {
        let effects = Arc::new(RecordingEffects::default());
        let coordinator = coordinator(effects.clone());

        coordinator.reconcile().await.expect("reconciled");

        assert_eq!(effects.clears(), 1);
        assert!(coordinator.snapshot().await.is_none());
    }
}
