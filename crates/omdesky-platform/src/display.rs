use async_trait::async_trait;
use omdesky_application::ports::{
    AgentClient, AgentEndpoint, DisplayTopologySource, PortError, PortResult, RemoteOmarchy,
    StreamDisplayController,
};
use omdesky_core::{
    Display, DisplayId, KeyChord, KeyModifier, RemoteCommand, RemoteDesktopTopology, WindowSelector,
};
use omdesky_protocol::CommandRequest;
use std::{path::Path, path::PathBuf, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
    sync::mpsc,
    task::JoinHandle,
    time::interval,
};

use crate::input::MOONLIGHT_WINDOW_CLASS;

const POLL_INTERVAL: Duration = Duration::from_millis(750);

const SUNSHINE_MAX_SWITCH_DISPLAYS: usize = 13;

const FOCUS_EVENT_PREFIXES: [&str; 9] = [
    "focusedmon",
    "workspace",
    "workspacev2",
    "moveworkspace",
    "moveworkspacev2",
    "monitoradded",
    "monitoraddedv2",
    "monitorremoved",
    "configreloaded",
];

#[derive(Clone)]
pub struct HyprlandDisplayTopology {
    desktop: Arc<dyn RemoteOmarchy>,
}

impl HyprlandDisplayTopology {
    pub fn new(desktop: Arc<dyn RemoteOmarchy>) -> Self {
        Self { desktop }
    }
}

#[async_trait]
impl DisplayTopologySource for HyprlandDisplayTopology {
    async fn topology(&self) -> PortResult<RemoteDesktopTopology> {
        let displays = self.desktop.displays().await?;
        Ok(RemoteDesktopTopology::from_displays(&displays))
    }
}

#[derive(Clone)]
pub struct MoonlightDisplayController {
    agent: Arc<dyn AgentClient>,
    controller: AgentEndpoint,
    desktop: Arc<dyn RemoteOmarchy>,
}

impl MoonlightDisplayController {
    pub fn new(
        agent: Arc<dyn AgentClient>,
        controller: AgentEndpoint,
        desktop: Arc<dyn RemoteOmarchy>,
    ) -> Self {
        Self {
            agent,
            controller,
            desktop,
        }
    }
}

#[async_trait]
impl StreamDisplayController for MoonlightDisplayController {
    async fn current_display(&self) -> PortResult<DisplayId> {
        let displays = self.desktop.displays().await?;

        RemoteDesktopTopology::from_displays(&displays)
            .preferred()
            .cloned()
            .ok_or_else(|| PortError::new("DISPLAY_NOT_FOUND", "no displays available", false))
    }

    async fn switch_display(&self, display: &DisplayId) -> PortResult<()> {
        let target = display;
        target
            .validate()
            .map_err(|error| PortError::new("INVALID_COMMAND", error.to_string(), false))?;

        tracing::debug!(
            display_id = %target,
            controller = %self.controller.address,
            "moonlight.switch_display.request"
        );

        let command = RemoteCommand::SwitchStreamDisplay {
            display: target.clone(),
        };

        self.agent
            .send_command(&self.controller, CommandRequest::new(command))
            .await
            .map(|_| ())
    }
}

pub fn resolve_display_switch_shortcut(
    displays: &[Display],
    target: &DisplayId,
) -> PortResult<RemoteCommand> {
    let index = display_switch_index(displays, target).ok_or_else(|| {
        PortError::new(
            "DISPLAY_NOT_FOUND",
            format!("display {target} is not part of the remote topology"),
            false,
        )
    })?;

    display_switch_shortcut(index)
}

pub fn display_switch_index(displays: &[Display], target: &DisplayId) -> Option<usize> {
    displays
        .iter()
        .position(|display| display.id == target.as_str())
}

fn display_switch_shortcut(index: usize) -> PortResult<RemoteCommand> {
    if index >= SUNSHINE_MAX_SWITCH_DISPLAYS {
        return Err(PortError::new(
            "DISPLAY_NOT_FOUND",
            "display index is beyond Sunshine's switchable range",
            false,
        ));
    }

    let chord = KeyChord::new(
        [KeyModifier::Ctrl, KeyModifier::Alt, KeyModifier::Shift],
        format!("F{}", index + 1),
    )
    .map_err(|error| PortError::new("INVALID_COMMAND", error.to_string(), false))?;

    Ok(RemoteCommand::SendShortcut {
        chord,
        window: WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned()),
    })
}

pub fn spawn_focus_signals(sender: mpsc::Sender<()>) -> JoinHandle<()> {
    tokio::spawn(async move {
        match event_socket_path() {
            Some(path) => {
                tracing::debug!(
                    path = %path.display(),
                    "follow_focus.hyprland_socket_connected"
                );
                if let Err(error) = forward_socket_events(&path, &sender).await {
                    tracing::debug!(
                        path = %path.display(),
                        detail = %error,
                        "follow_focus.hyprland_socket_lost"
                    );
                    poll_focus(&sender).await;
                }
            }
            None => {
                tracing::debug!("follow_focus.polling_fallback");
                poll_focus(&sender).await;
            }
        }
    })
}

fn event_socket_path() -> Option<PathBuf> {
    let signature = std::env::var("HYPRLAND_INSTANCE_SIGNATURE").ok()?;

    if !is_safe_instance_signature(&signature) {
        tracing::debug!("follow_focus.hyprland_signature_rejected");
        return None;
    }

    let runtime_dir = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)?;
    let candidate = runtime_dir.join(format!("hypr/{signature}/.socket2.sock"));

    is_own_unix_socket(&candidate).then_some(candidate)
}

fn is_safe_instance_signature(signature: &str) -> bool {
    !signature.is_empty()
        && signature.len() <= 128
        && !signature.contains("..")
        && signature.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
}

#[cfg(unix)]
fn is_own_unix_socket(path: &Path) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};

    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };

    let Ok(own_uid) = std::fs::metadata("/proc/self").map(|process| process.uid()) else {
        return false;
    };

    metadata.file_type().is_socket() && metadata.uid() == own_uid
}

#[cfg(not(unix))]
fn is_own_unix_socket(_path: &Path) -> bool {
    false
}

async fn forward_socket_events(path: &Path, sender: &mpsc::Sender<()>) -> std::io::Result<()> {
    let stream = UnixStream::connect(path).await?;
    let mut lines = BufReader::new(stream).lines();

    while let Some(line) = lines.next_line().await? {
        if is_focus_event(&line) && sender.send(()).await.is_err() {
            break;
        }
    }

    Ok(())
}

fn is_focus_event(line: &str) -> bool {
    let name = line.split_once(">>").map_or(line, |(name, _)| name);
    FOCUS_EVENT_PREFIXES.contains(&name)
}

async fn poll_focus(sender: &mpsc::Sender<()>) {
    let mut ticker = interval(POLL_INTERVAL);

    loop {
        ticker.tick().await;
        if sender.send(()).await.is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(id: &str) -> Display {
        Display {
            id: id.to_owned(),
            name: id.to_owned(),
            width: 1920,
            height: 1080,
            refresh_hz: 60.0,
            focused: false,
        }
    }

    #[test]
    fn test_display_maps_to_deterministic_switch_index() {
        let displays = [display("eDP-1"), display("DP-2"), display("HDMI-A-1")];

        assert_eq!(
            display_switch_index(&displays, &DisplayId::from("eDP-1")),
            Some(0)
        );
        assert_eq!(
            display_switch_index(&displays, &DisplayId::from("HDMI-A-1")),
            Some(2)
        );
        assert_eq!(
            display_switch_index(&displays, &DisplayId::from("absent")),
            None
        );
    }

    #[test]
    fn test_resolve_display_switch_shortcut_targets_moonlight_with_function_key() {
        let displays = [display("eDP-1"), display("DP-2"), display("HDMI-A-1")];
        let command = resolve_display_switch_shortcut(&displays, &DisplayId::from("DP-2"))
            .expect("valid display");

        match command {
            RemoteCommand::SendShortcut { chord, window } => {
                assert_eq!(chord.modifiers_hypr(), "CTRL ALT SHIFT");
                assert_eq!(chord.key(), "F2");
                assert_eq!(window.as_hypr(), format!("class:{MOONLIGHT_WINDOW_CLASS}"));
            }
            other => panic!("unexpected command: {other:?}"),
        }
    }

    #[test]
    fn test_resolve_display_switch_shortcut_rejects_absent_display() {
        let displays = [display("eDP-1")];
        assert_eq!(
            resolve_display_switch_shortcut(&displays, &DisplayId::from("absent"))
                .expect_err("absent display")
                .code,
            "DISPLAY_NOT_FOUND"
        );
    }

    #[test]
    fn test_resolve_display_switch_shortcut_rejects_index_beyond_sunshine_range() {
        let displays: Vec<Display> = (0..(SUNSHINE_MAX_SWITCH_DISPLAYS + 1))
            .map(|index| display(&format!("MON-{index}")))
            .collect();
        let target = DisplayId::from(format!("MON-{SUNSHINE_MAX_SWITCH_DISPLAYS}").as_str());

        assert_eq!(
            resolve_display_switch_shortcut(&displays, &target)
                .expect_err("out of range")
                .code,
            "DISPLAY_NOT_FOUND"
        );
    }

    #[test]
    fn test_instance_signature_rejects_path_and_control_characters() {
        assert!(is_safe_instance_signature("v0.41.2_1718000000"));
        assert!(is_safe_instance_signature(
            "efb50993780079460b0cbed1363e2166a2de1d9f_1790005601_1468723531"
        ));
        assert!(!is_safe_instance_signature("../../tmp/hypr"));
        assert!(!is_safe_instance_signature("sig/with/slash"));
        assert!(!is_safe_instance_signature("sig..sneaky"));
        assert!(!is_safe_instance_signature("sig\u{1b}[2J"));
        assert!(!is_safe_instance_signature(""));
        assert!(!is_safe_instance_signature(&"a".repeat(129)));
    }

    #[test]
    fn test_event_socket_rejects_a_regular_file_in_place_of_the_socket() {
        let directory = tempfile::tempdir().expect("temporary directory");
        let path = directory.path().join("not-a-socket");
        std::fs::write(&path, b"").expect("file written");

        assert!(!is_own_unix_socket(&path));
        assert!(!is_own_unix_socket(&directory.path().join("absent")));
    }

    #[test]
    fn test_focus_event_detection_matches_relevant_hyprland_events() {
        assert!(is_focus_event("focusedmon>>DP-2,3"));
        assert!(is_focus_event("workspace>>3"));
        assert!(is_focus_event("monitorremoved>>DP-2"));
        assert!(!is_focus_event("openwindow>>0x55,3,foot,shell"));
        assert!(!is_focus_event("activelayout>>keyboard,us"));
    }
}
