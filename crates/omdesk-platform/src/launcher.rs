use async_trait::async_trait;
use omdesk_application::ports::{LauncherSpec, LauncherStore, PortError, PortResult};
use omdesk_core::NodeId;
use std::{fs, path::PathBuf};

#[derive(Clone, Debug)]
pub struct DesktopLauncherStore {
    directory: PathBuf,
}

impl DesktopLauncherStore {
    pub fn new(directory: PathBuf) -> Self {
        Self { directory }
    }

    pub fn render(spec: &LauncherSpec) -> String {
        format!(
            "[Desktop Entry]\nName={}\nComment=Remote Omarchy Desktop\nExec=uwsm app -- omdesk connect {}\nIcon=computer\nTerminal=false\nType=Application\nCategories=Network;RemoteAccess;\nX-OmarchyDesk-NodeId={}\n",
            escape_value(&spec.display_name),
            spec.node_id,
            spec.node_id
        )
    }

    fn path(&self, node_id: NodeId) -> PathBuf {
        self.directory.join(format!("omdesk-{node_id}.desktop"))
    }
}

#[async_trait]
impl LauncherStore for DesktopLauncherStore {
    async fn create(&self, launcher: LauncherSpec) -> PortResult<PathBuf> {
        fs::create_dir_all(&self.directory).map_err(io_error)?;
        let path = self.path(launcher.node_id);
        let content = Self::render(&launcher);
        if fs::read_to_string(&path).ok().as_deref() != Some(&content) {
            fs::write(&path, content).map_err(io_error)?;
        }
        Ok(path)
    }

    async fn list(&self) -> PortResult<Vec<PathBuf>> {
        if !self.directory.exists() {
            return Ok(Vec::new());
        }
        let mut entries = fs::read_dir(&self.directory)
            .map_err(io_error)?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("omdesk-") && name.ends_with(".desktop"))
            })
            .collect::<Vec<_>>();
        entries.sort();
        Ok(entries)
    }

    async fn remove(&self, node_id: NodeId) -> PortResult<()> {
        let path = self.path(node_id);
        if path.exists() {
            fs::remove_file(path).map_err(io_error)?;
        }
        Ok(())
    }
}

fn escape_value(value: &str) -> String {
    value.replace(['\n', '\r'], " ")
}

fn io_error(error: std::io::Error) -> PortError {
    PortError::new("LAUNCHER_IO_FAILED", error.to_string(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_launcher_generation_is_idempotent_and_uses_node_id() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let store = DesktopLauncherStore::new(directory.path().to_path_buf());
        let node_id = NodeId::new();
        let spec = LauncherSpec {
            node_id,
            display_name: "Workstation".to_owned(),
            aliases: vec!["workstation".to_owned()],
        };

        let first = store.create(spec.clone()).await.expect("created");
        let second = store.create(spec).await.expect("created again");
        let content = fs::read_to_string(&first).expect("launcher content");

        assert_eq!(first, second);
        assert!(content.contains(&node_id.to_string()));
        assert!(!content.contains("100.64."));
    }
}
