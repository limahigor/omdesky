use async_trait::async_trait;
use omdesk_application::ports::{
    CommandRunner, CommandSpec, DesktopEnvironment, PortError, PortResult, RemoteOmarchy,
};
use omdesk_core::{Display, InputMode, Window, WindowId, Workspace, WorkspaceId, WorkspaceTarget};
use serde::Deserialize;
use std::sync::Arc;

#[derive(Clone)]
pub struct HyprlandAdapter {
    runner: Arc<dyn CommandRunner>,
}

impl HyprlandAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    async fn query(&self, resource: &str) -> PortResult<Vec<u8>> {
        Ok(self
            .runner
            .run(CommandSpec::new(
                "hyprctl",
                [resource.to_owned(), "-j".to_owned()],
            ))
            .await?
            .stdout)
    }

    /// Convenience probe for diagnostics without importing the port trait.
    pub async fn displays_probe(&self) -> PortResult<Vec<Display>> {
        RemoteOmarchy::displays(self).await
    }

    async fn dispatch(&self, args: Vec<String>) -> PortResult<()> {
        let mut argv = vec!["dispatch".to_owned()];
        argv.extend(args);
        self.runner
            .run(CommandSpec::new("hyprctl", argv))
            .await
            .map(|_| ())
            .map_err(|error| {
                PortError::new("HYPRLAND_DISPATCH_FAILED", error.message, error.retryable)
            })
    }
}

#[async_trait]
impl DesktopEnvironment for HyprlandAdapter {
    async fn active_display(&self) -> PortResult<Option<Display>> {
        let displays = RemoteOmarchy::displays(self).await?;
        Ok(displays
            .iter()
            .find(|display| display.focused)
            .cloned()
            .or_else(|| displays.into_iter().next()))
    }

    async fn set_input_mode(&self, _mode: InputMode) -> PortResult<()> {
        Ok(())
    }
}

#[async_trait]
impl RemoteOmarchy for HyprlandAdapter {
    async fn displays(&self) -> PortResult<Vec<Display>> {
        parse_monitors(&self.query("monitors").await?)
    }

    async fn workspaces(&self) -> PortResult<Vec<Workspace>> {
        parse_workspaces(&self.query("workspaces").await?)
    }

    async fn windows(&self) -> PortResult<Vec<Window>> {
        parse_windows(&self.query("clients").await?)
    }

    async fn active_window(&self) -> PortResult<Option<Window>> {
        parse_active_window(&self.query("activewindow").await?)
    }

    async fn focus_workspace(&self, target: WorkspaceTarget) -> PortResult<()> {
        let selector = match target {
            WorkspaceTarget::Id(id) => id.0.to_string(),
            WorkspaceTarget::Name(name) => {
                if name.is_empty() || name.contains(char::is_whitespace) {
                    return Err(PortError::new(
                        "WORKSPACE_NOT_FOUND",
                        "workspace name is not a safe selector",
                        false,
                    ));
                }
                format!("name:{name}")
            }
        };

        self.dispatch(vec!["workspace".to_owned(), selector]).await
    }

    async fn focus_window(&self, id: &str) -> PortResult<()> {
        if !is_window_handle(id) {
            return Err(PortError::new(
                "WINDOW_NOT_FOUND",
                "window handle is not a valid Hyprland address",
                false,
            ));
        }

        self.dispatch(vec!["focuswindow".to_owned(), format!("address:{id}")])
            .await
    }
}

/// A Hyprland window handle is an address such as `0x55c9ab12`.
fn is_window_handle(value: &str) -> bool {
    value
        .strip_prefix("0x")
        .is_some_and(|hex| !hex.is_empty() && hex.chars().all(|c| c.is_ascii_hexdigit()))
}

pub fn parse_monitors(bytes: &[u8]) -> PortResult<Vec<Display>> {
    let monitors: Vec<HyprMonitor> = serde_json::from_slice(bytes).map_err(hyprland_error)?;
    Ok(monitors
        .into_iter()
        .filter(|monitor| !monitor.disabled)
        .map(|monitor| Display {
            id: monitor.name.clone(),
            name: monitor.description.unwrap_or(monitor.name),
            width: monitor.width,
            height: monitor.height,
            refresh_hz: monitor.refresh_rate,
            focused: monitor.focused,
        })
        .collect())
}

pub fn parse_workspaces(bytes: &[u8]) -> PortResult<Vec<Workspace>> {
    let mut workspaces: Vec<HyprWorkspace> =
        serde_json::from_slice(bytes).map_err(hyprland_error)?;
    workspaces.sort_by_key(|workspace| workspace.id);
    Ok(workspaces
        .into_iter()
        .map(|workspace| Workspace {
            id: WorkspaceId(workspace.id),
            name: workspace.name,
            monitor: workspace.monitor,
            focused: false,
            windows: workspace.windows,
        })
        .collect())
}

pub fn parse_windows(bytes: &[u8]) -> PortResult<Vec<Window>> {
    let clients: Vec<HyprClient> = serde_json::from_slice(bytes).map_err(hyprland_error)?;
    Ok(clients.into_iter().map(client_to_window).collect())
}

pub fn parse_active_window(bytes: &[u8]) -> PortResult<Option<Window>> {
    let trimmed = std::str::from_utf8(bytes).unwrap_or_default().trim();

    if trimmed.is_empty() || trimmed == "{}" {
        return Ok(None);
    }

    let client: HyprClient = serde_json::from_slice(bytes).map_err(hyprland_error)?;

    if client.address.is_empty() {
        return Ok(None);
    }

    Ok(Some(client_to_window(client)))
}

fn client_to_window(client: HyprClient) -> Window {
    Window {
        id: WindowId(client.address),
        app_id: client
            .initial_class
            .clone()
            .filter(|value| !value.is_empty()),
        class: client.class.filter(|value| !value.is_empty()),
        title: client.title.filter(|value| !value.is_empty()),
        workspace: WorkspaceId(client.workspace.id),
        focused: false,
    }
}

fn hyprland_error(error: serde_json::Error) -> PortError {
    PortError::new(
        "HYPRLAND_UNAVAILABLE",
        format!("unable to parse Hyprland output: {error}"),
        false,
    )
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprMonitor {
    name: String,
    description: Option<String>,
    width: u32,
    height: u32,
    refresh_rate: f64,
    #[serde(default)]
    focused: bool,
    #[serde(default)]
    disabled: bool,
}

#[derive(Debug, Deserialize)]
struct HyprWorkspace {
    id: i64,
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    monitor: Option<String>,
    #[serde(default)]
    windows: u32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct HyprClient {
    address: String,
    #[serde(default)]
    class: Option<String>,
    #[serde(default)]
    initial_class: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    workspace: HyprClientWorkspace,
}

#[derive(Debug, Default, Deserialize)]
struct HyprClientWorkspace {
    #[serde(default)]
    id: i64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_monitors_ignores_disabled_outputs() {
        let displays = parse_monitors(
            br#"[{"name":"DP-1","description":"Main","width":2560,"height":1440,"refreshRate":165.0,"focused":true},{"name":"HDMI-A-1","width":1920,"height":1080,"refreshRate":60.0,"disabled":true}]"#,
        )
        .expect("monitors parse");

        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].id, "DP-1");
        assert!(displays[0].focused);
    }

    #[test]
    fn test_parse_workspaces_orders_by_id() {
        let workspaces = parse_workspaces(
            br#"[{"id":3,"name":"Media","monitor":"DP-1","windows":1},{"id":1,"name":"Web","monitor":"DP-1","windows":2}]"#,
        )
        .expect("workspaces parse");

        assert_eq!(workspaces[0].id, WorkspaceId(1));
        assert_eq!(workspaces[1].id, WorkspaceId(3));
        assert_eq!(workspaces[0].windows, 2);
    }

    #[test]
    fn test_parse_windows_maps_stable_handle_and_workspace() {
        let windows = parse_windows(
            br#"[{"address":"0x55c9","class":"firefox","initialClass":"firefox","title":"News","workspace":{"id":1}}]"#,
        )
        .expect("windows parse");

        assert_eq!(windows[0].id, WindowId("0x55c9".to_owned()));
        assert_eq!(windows[0].app_id.as_deref(), Some("firefox"));
        assert_eq!(windows[0].workspace, WorkspaceId(1));
    }

    #[test]
    fn test_parse_active_window_handles_empty() {
        assert_eq!(parse_active_window(b"{}").expect("empty"), None);
    }

    #[test]
    fn test_window_handle_validation_rejects_injection() {
        assert!(is_window_handle("0x55c9ab"));
        assert!(!is_window_handle("0x55; rm -rf"));
        assert!(!is_window_handle("firefox"));
    }
}
