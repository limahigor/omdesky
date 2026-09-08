use async_trait::async_trait;
use omdesk_application::ports::{
    AgentEndpoint, CommandExecutor, CommandRunner, CommandSpec, PortError, PortResult,
    SessionKeybindConfig, SessionKeybindInstaller,
};
use omdesk_core::{KeyChord, KeyModifier, RemoteCommand, SessionRole, WindowSelector};
use std::sync::Arc;

const MOONLIGHT_WINDOW_CLASS: &str = "com.moonlight_stream.Moonlight";
const BINDS_GLOBAL: &str = "_G.omdesk_binds";

struct Keybind {
    trigger: &'static str,
    description: &'static str,
    command: RemoteCommand,
    allow_input_capture: bool,
}

fn session_keybinds() -> Vec<Keybind> {
    vec![
        Keybind {
            trigger: "SUPER + R",
            description: "toggle remote shortcut capture",
            command: moonlight_shortcut("Z"),
            allow_input_capture: true,
        },
        Keybind {
            trigger: "SUPER + Q",
            description: "close remote session",
            command: moonlight_close_command(),
            allow_input_capture: true,
        },
    ]
}

fn moonlight_shortcut(key: &str) -> RemoteCommand {
    let chord = KeyChord::new(
        [KeyModifier::Ctrl, KeyModifier::Alt, KeyModifier::Shift],
        key,
    )
    .expect("static Moonlight chord is valid");

    RemoteCommand::SendShortcut {
        chord,
        window: WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned()),
    }
}

fn moonlight_close_command() -> RemoteCommand {
    RemoteCommand::CloseWindow {
        window: WindowSelector::Class(MOONLIGHT_WINDOW_CLASS.to_owned()),
    }
}

pub struct HyprlandCommandExecutor {
    runner: Arc<dyn CommandRunner>,
}

impl HyprlandCommandExecutor {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl CommandExecutor for HyprlandCommandExecutor {
    async fn execute(&self, command: RemoteCommand) -> PortResult<()> {
        command
            .validate()
            .map_err(|error| PortError::new("INVALID_COMMAND", error.to_string(), false))?;

        let lua = command_dispatch_lua(&command).ok_or_else(|| {
            PortError::new("COMMAND_NOT_EXECUTABLE", "command is not an action", false)
        })?;

        let spec = CommandSpec::new("hyprctl", ["eval".to_owned(), lua]);

        self.runner
            .run(spec)
            .await
            .map(|_| ())
            .map_err(|error| PortError::new("HYPRLAND_UNAVAILABLE", error.message, error.retryable))
    }
}

pub struct HyprlandSessionKeybinds {
    runner: Arc<dyn CommandRunner>,
}

impl HyprlandSessionKeybinds {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

#[async_trait]
impl SessionKeybindInstaller for HyprlandSessionKeybinds {
    async fn install(&self, config: SessionKeybindConfig) -> PortResult<()> {
        let spec = install_session_keybinds_spec(config.role, config.controller.as_ref());

        let _ = self.runner.run(spec).await;

        Ok(())
    }

    async fn clear(&self) -> PortResult<()> {
        let _ = self.runner.run(remove_session_keybinds_spec()).await;

        Ok(())
    }
}

fn command_dispatch_lua(command: &RemoteCommand) -> Option<String> {
    match command {
        RemoteCommand::SendShortcut { chord, window } => Some(format!(
            "hl.dispatch(hl.dsp.send_shortcut({{ mods = \"{}\", key = \"{}\", window = \"{}\" }}))",
            chord.modifiers_hypr(),
            chord.key(),
            window.as_hypr()
        )),
        RemoteCommand::CloseWindow { window } => Some(format!(
            "hl.dispatch(hl.dsp.window.close({{ window = \"{}\" }}))",
            window.as_hypr()
        )),
        RemoteCommand::AttachSession { .. } | RemoteCommand::DetachSession => None,
    }
}

fn command_cli_invocation(command: &RemoteCommand, endpoint: &AgentEndpoint) -> Option<String> {
    match command {
        RemoteCommand::SendShortcut { chord, window } => {
            let window_arg = match window {
                WindowSelector::Class(class) => format!(" --window-class {class}"),
                WindowSelector::ActiveWindow => String::new(),
            };

            Some(format!(
                "omdesk command send-shortcut {address} --port {port} --mods '{mods}' --key {key}{window_arg}",
                address = endpoint.address,
                port = endpoint.port,
                mods = chord.modifiers_hypr(),
                key = chord.key(),
            ))
        }
        RemoteCommand::CloseWindow { window } => {
            let window_arg = match window {
                WindowSelector::Class(class) => format!(" --window-class {class}"),
                WindowSelector::ActiveWindow => String::new(),
            };

            Some(format!(
                "omdesk command close-window {address} --port {port}{window_arg}",
                address = endpoint.address,
                port = endpoint.port,
            ))
        }
        RemoteCommand::AttachSession { .. } | RemoteCommand::DetachSession => None,
    }
}

fn notify(message: &str) -> String {
    format!("hl.dispatch(hl.dsp.exec_cmd(\"omarchy-notification-send DeskLink '{message}'\"))")
}

fn controller_action_lua(bind: &Keybind) -> Option<String> {
    let dispatch = command_dispatch_lua(&bind.command)?;
    let notify = notify(bind.description);

    Some(format!("function() {dispatch}; {notify} end"))
}

fn remote_action_lua(bind: &Keybind, controller: &AgentEndpoint) -> Option<String> {
    let invocation = command_cli_invocation(&bind.command, controller)?;
    let shell = format!(
        "log=\\\"${{XDG_RUNTIME_DIR:-/tmp}}/omdesk-keybind.log\\\"; \
{invocation} >\\\"$log\\\" 2>&1 && \
omarchy-notification-send DeskLink '{ok}' || \
omarchy-notification-send DeskLink \\\"DeskLink failed: $(cat \\\"$log\\\")\\\"",
        ok = bind.description,
    );

    Some(format!("hl.dsp.exec_cmd(\"{shell}\")"))
}

fn install_session_keybinds_spec(
    role: SessionRole,
    controller: Option<&AgentEndpoint>,
) -> CommandSpec {
    let mut lua = format!(
        "if {BINDS_GLOBAL} then for _, b in ipairs({BINDS_GLOBAL}) do b:remove() end end; \
{BINDS_GLOBAL} = {{}}"
    );

    for bind in session_keybinds() {
        if role == SessionRole::Remote && bind.command.controller_exclusive() {
            continue;
        }

        let action = match role {
            SessionRole::Controller => controller_action_lua(&bind),
            SessionRole::Remote => match controller {
                Some(controller) => remote_action_lua(&bind, controller),
                None => None,
            },
        };

        let Some(action) = action else {
            continue;
        };

        lua.push_str(&format!(
            "; table.insert({BINDS_GLOBAL}, hl.bind(\"{trigger}\", {action}, \
{{ allow_input_capture = {capture}, description = \"DeskLink: {description}\" }}))",
            trigger = bind.trigger,
            description = bind.description,
            capture = bind.allow_input_capture,
        ));
    }

    CommandSpec::new("hyprctl", ["eval".to_owned(), lua])
}

fn remove_session_keybinds_spec() -> CommandSpec {
    let lua = format!(
        "if {BINDS_GLOBAL} then for _, b in ipairs({BINDS_GLOBAL}) do b:remove() end end; \
{BINDS_GLOBAL} = nil"
    );

    CommandSpec::new("hyprctl", ["eval".to_owned(), lua])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn controller() -> AgentEndpoint {
        AgentEndpoint {
            address: "100.64.0.7".parse().expect("valid address"),
            port: 8765,
        }
    }

    #[test]
    fn test_controller_bind_dispatches_directly_without_http() {
        let spec = install_session_keybinds_spec(SessionRole::Controller, None);

        assert_eq!(spec.program, "hyprctl");
        assert!(spec.args[1].contains("SUPER + R"));
        assert!(spec.args[1].contains("SUPER + Q"));
        assert!(spec.args[1].contains("hl.dsp.send_shortcut"));
        assert!(spec.args[1].contains("class:com.moonlight_stream.Moonlight"));
        assert!(!spec.args[1].contains("omdesk command"));
        assert!(spec.args[1].contains("omarchy-notification-send DeskLink"));
    }

    #[test]
    fn test_close_session_bind_closes_moonlight_window() {
        let controller_spec = install_session_keybinds_spec(SessionRole::Controller, None);
        assert!(controller_spec.args[1].contains(
            "hl.dsp.window.close({ window = \"class:com.moonlight_stream.Moonlight\" })"
        ));

        let remote = install_session_keybinds_spec(SessionRole::Remote, Some(&controller()));
        assert!(remote.args[1].contains("omdesk command close-window 100.64.0.7 --port 8765"));
        assert!(remote.args[1].contains("--window-class com.moonlight_stream.Moonlight"));
        assert!(remote.args[1].contains("close remote session"));
    }

    #[test]
    fn test_remote_bind_forwards_to_controller_with_diagnostic_notify() {
        let spec = install_session_keybinds_spec(SessionRole::Remote, Some(&controller()));

        assert!(spec.args[1].contains("SUPER + R"));
        assert!(spec.args[1].contains("omdesk command send-shortcut 100.64.0.7 --port 8765"));
        assert!(spec.args[1].contains("--mods 'CTRL ALT SHIFT' --key Z"));
        assert!(spec.args[1].contains("--window-class com.moonlight_stream.Moonlight"));
        assert!(spec.args[1].contains("&& omarchy-notification-send DeskLink"));
        assert!(spec.args[1].contains("|| omarchy-notification-send DeskLink"));
        assert!(spec.args[1].contains("${XDG_RUNTIME_DIR:-/tmp}/omdesk-keybind.log"));
    }

    #[test]
    fn test_install_clears_prior_bindings_before_registering() {
        let spec = install_session_keybinds_spec(SessionRole::Controller, None);

        let clear = spec.args[1].find(":remove()").expect("clears prior binds");
        let register = spec.args[1].find("hl.bind(").expect("registers binds");
        assert!(clear < register);
    }

    #[test]
    fn test_remote_install_without_controller_registers_nothing() {
        let spec = install_session_keybinds_spec(SessionRole::Remote, None);

        assert!(!spec.args[1].contains("hl.bind"));
    }

    #[test]
    fn test_remove_clears_only_desklink_bindings() {
        let spec = remove_session_keybinds_spec();

        assert!(spec.args[1].contains("omdesk_binds"));
        assert!(spec.args[1].contains(":remove()"));
        assert!(!spec.args[1].contains("hl.unbind"));
    }

    #[test]
    fn test_command_dispatch_lua_uses_validated_tokens() {
        let lua = command_dispatch_lua(&moonlight_shortcut("Z")).expect("action renders");

        assert_eq!(
            lua,
            "hl.dispatch(hl.dsp.send_shortcut({ mods = \"CTRL ALT SHIFT\", key = \"Z\", \
window = \"class:com.moonlight_stream.Moonlight\" }))"
        );
    }

    #[test]
    fn test_session_commands_are_not_executable_actions() {
        assert!(command_dispatch_lua(&RemoteCommand::DetachSession).is_none());
    }
}
