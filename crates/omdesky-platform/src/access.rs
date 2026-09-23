use crate::state::{atomic_write_json, read_json};
use async_trait::async_trait;
use omdesky_application::ports::{AccessStore, AllowedController, PortError, PortResult};
use std::{path::PathBuf, sync::Arc};
use tokio::sync::Mutex;

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

    fn read_blocking(path: &std::path::Path) -> PortResult<Vec<AllowedController>> {
        match read_json(path) {
            Ok(controllers) => Ok(controllers),
            Err(crate::state::StateError::Io(error))
                if error.kind() == std::io::ErrorKind::NotFound =>
            {
                Ok(Vec::new())
            }
            Err(error) => Err(state_error(error)),
        }
    }

    async fn read_all(&self) -> PortResult<Vec<AllowedController>> {
        let path = self.path.clone();

        tokio::task::spawn_blocking(move || Self::read_blocking(&path))
            .await
            .map_err(|_| state_error("the allowlist could not be read"))?
    }

    async fn write_all(&self, controllers: Vec<AllowedController>) -> PortResult<()> {
        let path = self.path.clone();

        tokio::task::spawn_blocking(move || {
            atomic_write_json(&path, &controllers, true).map_err(state_error)
        })
        .await
        .map_err(|_| state_error("the allowlist could not be written"))?
    }
}

#[async_trait]
impl AccessStore for FileAccessStore {
    async fn list(&self) -> PortResult<Vec<AllowedController>> {
        let _guard = self.lock.lock().await;

        self.read_all().await
    }

    async fn allow(&self, controller: AllowedController) -> PortResult<()> {
        let _guard = self.lock.lock().await;

        let mut controllers = self.read_all().await?;
        controllers.retain(|candidate| candidate.tailnet_node_id != controller.tailnet_node_id);
        controllers.push(controller);

        self.write_all(controllers).await
    }

    async fn revoke(&self, tailnet_node_id: &str) -> PortResult<()> {
        let _guard = self.lock.lock().await;

        let mut controllers = self.read_all().await?;
        controllers.retain(|candidate| candidate.tailnet_node_id != tailnet_node_id);

        self.write_all(controllers).await
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

        assert_eq!(store.list().await.expect("lookup").len(), 1);
        store.revoke("node-abc").await.expect("identity revoked");
        assert!(store.list().await.expect("lookup").is_empty());
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
