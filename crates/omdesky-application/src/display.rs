use crate::ports::{
    DisplayTopologySource, Notification, NotificationService, StreamDisplayController,
};
use omdesky_core::{DisplayId, follow_focus_target};
use std::{future, sync::Arc, time::Duration};
use tokio::{
    sync::mpsc,
    time::{Instant, sleep_until},
};

pub struct FollowFocusRouter {
    controller: Arc<dyn StreamDisplayController>,
    topology: Arc<dyn DisplayTopologySource>,
    notifications: Arc<dyn NotificationService>,
    debounce: Duration,
}

struct RoutingState {
    streamed: Option<DisplayId>,
    pending: Option<DisplayId>,
    deadline: Option<Instant>,
}

impl FollowFocusRouter {
    pub fn new(
        controller: Arc<dyn StreamDisplayController>,
        topology: Arc<dyn DisplayTopologySource>,
        notifications: Arc<dyn NotificationService>,
        debounce: Duration,
    ) -> Self {
        Self {
            controller,
            topology,
            notifications,
            debounce,
        }
    }

    pub async fn run(self, mut signals: mpsc::Receiver<()>) {
        let streamed = match self.controller.current_display().await {
            Ok(display) => Some(display),
            Err(error) => {
                tracing::debug!(
                    code = error.code,
                    detail = %error.message,
                    "follow_focus.initial_display_unavailable"
                );
                None
            }
        };
        let mut state = RoutingState {
            streamed,
            pending: None,
            deadline: None,
        };

        loop {
            let wait_switch = async {
                match state.deadline {
                    Some(deadline) => sleep_until(deadline).await,
                    None => future::pending::<()>().await,
                }
            };

            tokio::select! {
                signal = signals.recv() => {
                    match signal {
                        Some(()) => self.reevaluate(&mut state).await,
                        None => break,
                    }
                }
                () = wait_switch => self.commit_pending(&mut state).await,
            }
        }
    }

    async fn reevaluate(&self, state: &mut RoutingState) {
        let topology = match self.topology.topology().await {
            Ok(topology) => topology,
            Err(error) => {
                tracing::debug!(
                    code = error.code,
                    detail = %error.message,
                    "follow_focus.topology_unavailable"
                );
                return;
            }
        };

        let Some(streamed) = state
            .streamed
            .clone()
            .or_else(|| topology.preferred().cloned())
        else {
            tracing::debug!("follow_focus.no_streamed_display");
            return;
        };
        state.streamed = Some(streamed.clone());

        match follow_focus_target(&topology, &streamed) {
            Some(target) => {
                tracing::debug!(
                    target_display = %target,
                    streamed_display = %streamed,
                    "follow_focus.switch_pending"
                );
                state.pending = Some(target);
                state.deadline = Some(Instant::now() + self.debounce);
            }
            None => {
                state.pending = None;
                state.deadline = None;
            }
        }
    }

    async fn commit_pending(&self, state: &mut RoutingState) {
        state.deadline = None;
        let Some(target) = state.pending.take() else {
            return;
        };

        tracing::debug!(target_display = %target, "follow_focus.switch_committing");

        match self.controller.switch_display(&target).await {
            Ok(()) => {
                tracing::debug!(target_display = %target, "follow_focus.switch_ok");
                state.streamed = Some(target);
            }
            Err(error) => {
                tracing::debug!(
                    code = error.code,
                    target_display = %target,
                    message = %error.message,
                    "follow_focus.switch_failed"
                );
                let _ = self
                    .notifications
                    .send(Notification {
                        summary: "Omdesky".to_owned(),
                        body: "The streamed display could not be changed. The current display will stay active."
                            .to_owned(),
                    })
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ports::{PortError, PortResult};
    use async_trait::async_trait;
    use omdesky_core::RemoteDesktopTopology;
    use std::sync::Mutex;
    use tokio::time::{Duration, advance};

    struct FakeController {
        initial: DisplayId,
        switches: Mutex<Vec<DisplayId>>,
        fail_next: Mutex<bool>,
    }

    impl FakeController {
        fn new(initial: &str) -> Arc<Self> {
            Arc::new(Self {
                initial: DisplayId::from(initial),
                switches: Mutex::new(Vec::new()),
                fail_next: Mutex::new(false),
            })
        }

        fn fail_once(&self) {
            *self.fail_next.lock().expect("lock") = true;
        }

        fn switches(&self) -> Vec<DisplayId> {
            self.switches.lock().expect("lock").clone()
        }
    }

    #[async_trait]
    impl StreamDisplayController for FakeController {
        async fn current_display(&self) -> PortResult<DisplayId> {
            Ok(self.initial.clone())
        }

        async fn switch_display(&self, display: &DisplayId) -> PortResult<()> {
            let mut fail = self.fail_next.lock().expect("lock");
            if *fail {
                *fail = false;
                return Err(PortError::new(
                    "DISPLAY_SWITCH_FAILED",
                    "backend refused",
                    true,
                ));
            }
            self.switches.lock().expect("lock").push(display.clone());
            Ok(())
        }
    }

    struct FakeTopology {
        current: Mutex<RemoteDesktopTopology>,
    }

    impl FakeTopology {
        fn new(displays: &[&str], focused: Option<&str>) -> Arc<Self> {
            Arc::new(Self {
                current: Mutex::new(build_topology(displays, focused)),
            })
        }

        fn set(&self, displays: &[&str], focused: Option<&str>) {
            *self.current.lock().expect("lock") = build_topology(displays, focused);
        }
    }

    fn build_topology(displays: &[&str], focused: Option<&str>) -> RemoteDesktopTopology {
        RemoteDesktopTopology::new(
            displays.iter().map(|id| DisplayId::from(*id)),
            focused.map(DisplayId::from),
        )
    }

    #[async_trait]
    impl DisplayTopologySource for FakeTopology {
        async fn topology(&self) -> PortResult<RemoteDesktopTopology> {
            Ok(self.current.lock().expect("lock").clone())
        }
    }

    struct SilentNotifications;

    #[async_trait]
    impl NotificationService for SilentNotifications {
        async fn send(&self, _notification: Notification) -> PortResult<()> {
            Ok(())
        }
    }

    fn router(controller: Arc<FakeController>, topology: Arc<FakeTopology>) -> FollowFocusRouter {
        FollowFocusRouter::new(
            controller,
            topology,
            Arc::new(SilentNotifications),
            Duration::from_millis(150),
        )
    }

    async fn settle() {
        for _ in 0..16 {
            tokio::task::yield_now().await;
        }
    }

    #[tokio::test(start_paused = true)]
    async fn test_same_display_focus_does_not_switch() {
        let controller = FakeController::new("eDP-1");
        let topology = FakeTopology::new(&["eDP-1", "DP-2"], Some("eDP-1"));
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(router(controller.clone(), topology).run(rx));

        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        drop(tx);
        handle.await.expect("join");

        assert!(controller.switches().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn test_focus_change_switches_once() {
        let controller = FakeController::new("eDP-1");
        let topology = FakeTopology::new(&["eDP-1", "DP-2"], Some("DP-2"));
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(router(controller.clone(), topology).run(rx));

        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        drop(tx);
        handle.await.expect("join");

        assert_eq!(controller.switches(), vec![DisplayId::from("DP-2")]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_duplicate_event_does_not_switch_twice() {
        let controller = FakeController::new("eDP-1");
        let topology = FakeTopology::new(&["eDP-1", "DP-2"], Some("DP-2"));
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(router(controller.clone(), topology).run(rx));

        tx.send(()).await.expect("send");
        settle().await;
        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        drop(tx);
        handle.await.expect("join");

        assert_eq!(controller.switches(), vec![DisplayId::from("DP-2")]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_rapid_changes_back_to_origin_are_deduplicated() {
        let controller = FakeController::new("eDP-1");
        let topology = FakeTopology::new(&["eDP-1", "DP-2"], Some("DP-2"));
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(router(controller.clone(), topology.clone()).run(rx));

        tx.send(()).await.expect("send");
        settle().await;
        topology.set(&["eDP-1", "DP-2"], Some("eDP-1"));
        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        drop(tx);
        handle.await.expect("join");

        assert!(controller.switches().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn test_switch_failure_keeps_session_and_allows_retry() {
        let controller = FakeController::new("eDP-1");
        let topology = FakeTopology::new(&["eDP-1", "DP-2"], Some("DP-2"));
        controller.fail_once();
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(router(controller.clone(), topology.clone()).run(rx));

        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        assert!(controller.switches().is_empty());

        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        drop(tx);
        handle.await.expect("join");

        assert_eq!(controller.switches(), vec![DisplayId::from("DP-2")]);
    }

    #[tokio::test(start_paused = true)]
    async fn test_disappearing_display_falls_back_to_focused_monitor() {
        let controller = FakeController::new("DP-2");
        let topology = FakeTopology::new(&["eDP-1"], Some("eDP-1"));
        let (tx, rx) = mpsc::channel(8);
        let handle = tokio::spawn(router(controller.clone(), topology).run(rx));

        tx.send(()).await.expect("send");
        settle().await;
        advance(Duration::from_millis(300)).await;
        settle().await;
        drop(tx);
        handle.await.expect("join");

        assert_eq!(controller.switches(), vec![DisplayId::from("eDP-1")]);
    }
}
