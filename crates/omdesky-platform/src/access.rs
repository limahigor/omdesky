use crate::state::{atomic_write_json, read_json};
use async_trait::async_trait;
use omdesky_application::ports::{AccessStore, AllowedController, PortError, PortResult};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

/// File-backed authorization allowlist keyed on Tailscale identity. Trust flows
/// from Tailscale; this only narrows which Tailnet identities may drive the
/// agent's control actions.
#[derive(Clone)]
pub struct FileAccessStore {
    path: PathBuf,
    lock: Arc<Mutex<()>>,
}

impl FileAccessStore {
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            lock: Arc::new(Mutex::new(())),
        }
    }

    fn read_all(&self) -> PortResult<Vec<AllowedController>> {
        if !self.path.exists() {
            return Ok(Vec::new());
        }

        read_json(&self.path).map_err(state_error)
    }

    fn write_all(&self, controllers: &[AllowedController]) -> PortResult<()> {
        atomic_write_json(&self.path, controllers, false).map_err(state_error)
    }
}

#[async_trait]
impl AccessStore for FileAccessStore {
    async fn list(&self) -> PortResult<Vec<AllowedController>> {
        let _guard = self.lock.lock().await;
        self.read_all()
    }

    async fn allow(&self, controller: AllowedController) -> PortResult<()> {
        let _guard = self.lock.lock().await;
        let mut controllers = self.read_all()?;
        controllers.retain(|candidate| candidate.tailnet_node_id != controller.tailnet_node_id);
        controllers.push(controller);

        self.write_all(&controllers)
    }

    async fn revoke(&self, tailnet_node_id: &str) -> PortResult<()> {
        let _guard = self.lock.lock().await;
        let mut controllers = self.read_all()?;
        controllers.retain(|candidate| candidate.tailnet_node_id != tailnet_node_id);

        self.write_all(&controllers)
    }

    async fn is_allowed(&self, tailnet_node_id: &str) -> PortResult<bool> {
        let _guard = self.lock.lock().await;
        Ok(self
            .read_all()?
            .iter()
            .any(|candidate| candidate.tailnet_node_id == tailnet_node_id))
    }
}

fn state_error(error: impl std::fmt::Display) -> PortError {
    PortError::new("ACCESS_STORE_FAILED", error.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesky_core::ControlCapability;
    use time::OffsetDateTime;

    #[tokio::test]
    async fn test_allowlist_persists_and_revokes_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = FileAccessStore::new(directory.path().join("access/allowlist.json"));
        store
            .allow(AllowedController::new(
                "node-abc",
                Some("desktop-a".to_owned()),
                OffsetDateTime::UNIX_EPOCH,
                ControlCapability::ALL,
            ))
            .await
            .expect("identity allowed");

        assert!(store.is_allowed("node-abc").await.expect("lookup"));
        store.revoke("node-abc").await.expect("identity revoked");
        assert!(!store.is_allowed("node-abc").await.expect("lookup"));
    }

    #[tokio::test]
    async fn test_allow_is_idempotent_per_identity() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = FileAccessStore::new(directory.path().join("access/allowlist.json"));
        let controller = AllowedController::new(
            "node-abc",
            None,
            OffsetDateTime::UNIX_EPOCH,
            ControlCapability::ALL,
        );
        store.allow(controller.clone()).await.expect("first");
        store.allow(controller).await.expect("second");

        assert_eq!(store.list().await.expect("list").len(), 1);
    }
}
