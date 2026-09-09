use async_trait::async_trait;
use omdesk_application::ports::{
    AgentEndpoint, CommandExecutor, CommandRunner, CommandSpec, PortError, PortResult,
    SessionKeybindConfig, SessionKeybindInstaller,
};
use omdesk_core::{KeyChord, KeyModifier, RemoteCommand, SessionRole, WindowSelector};
use std::sync::Arc;

pub const MOONLIGHT_WINDOW_CLASS: &str = "com.moonlight_stream.Moonlight";
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

    async fn run_hyprland(&self, spec: CommandSpec) -> PortResult<Vec<u8>> {
        let output = self.runner.run(spec).await.map_err(|error| {
            PortError::new("HYPRLAND_UNAVAILABLE", error.message, error.retryable)
        })?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let combined = format!("{stdout}\n{stderr}");

        tracing::debug!(output = %combined.trim(), "hyprland.command.output");

        if combined.contains("window not found") || combined.contains("not found") {
            return Err(PortError::new(
                "HYPRLAND_TARGET_MISSING",
                format!("Hyprland reported: {}", combined.trim()),
                false,
            ));
        }

        Ok(output.stdout)
    }

    async fn eval(&self, dispatcher: String) -> PortResult<()> {
        let lua = format!("hl.dispatch({dispatcher})");
        self.run_hyprland(CommandSpec::new("hyprctl", ["eval".to_owned(), lua]))
            .await
            .map(|_| ())
    }

    async fn send_shortcut(&self, chord: &KeyChord, window: &WindowSelector) -> PortResult<()> {
        let previous = match window {
            WindowSelector::ActiveWindow => None,
            WindowSelector::Class(_) => {
                let output = self
                    .run_hyprland(CommandSpec::new(
                        "hyprctl",
                        ["activewindow".to_owned(), "-j".to_owned()],
                    ))
                    .await?;
                Some(active_window_address(&output)?)
            }
        };

        let mut first_error = match window {
            WindowSelector::ActiveWindow => None,
            WindowSelector::Class(_) => self
                .eval(format!(
                    "hl.dsp.focus({{ window = \"{}\" }})",
                    window.as_hypr()
                ))
                .await
                .err(),
        };
        let mut pressed_modifiers = Vec::new();
        let mut key_attempted = false;

        if first_error.is_none() {
            for modifier in chord.modifiers() {
                let key = modifier_key(*modifier);
                let result = self.eval(key_state_dispatcher(key, "down")).await;
                pressed_modifiers.push(key);

                if let Err(error) = result {
                    first_error = Some(error);
                    break;
                }
            }
        }

        if first_error.is_none() {
            key_attempted = true;
            if let Err(error) = self.eval(key_state_dispatcher(chord.key(), "down")).await {
                first_error = Some(error);
            }
        }

        if key_attempted
            && let Err(error) = self.eval(key_state_dispatcher(chord.key(), "up")).await
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        for modifier in pressed_modifiers.into_iter().rev() {
            if let Err(error) = self.eval(key_state_dispatcher(modifier, "up")).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }

        if let Some(previous) = previous
            && let Err(error) = self
                .eval(format!(
                    "hl.dsp.focus({{ window = \"address:{previous}\" }})"
                ))
                .await
            && first_error.is_none()
        {
            first_error = Some(error);
        }

        match first_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

#[async_trait]
impl CommandExecutor for HyprlandCommandExecutor {
    async fn execute(&self, command: RemoteCommand) -> PortResult<()> {
        command
            .validate()
            .map_err(|error| PortError::new("INVALID_COMMAND", error.to_string(), false))?;

        match &command {
            RemoteCommand::SendShortcut { chord, window } => {
                self.send_shortcut(chord, window).await
            }
            RemoteCommand::CloseWindow { .. } => {
                let dispatcher = command_dispatcher(&command).ok_or_else(|| {
                    PortError::new("COMMAND_NOT_EXECUTABLE", "command is not an action", false)
                })?;
                self.eval(dispatcher).await
            }
            RemoteCommand::SwitchStreamDisplay { .. }
            | RemoteCommand::AttachSession { .. }
            | RemoteCommand::DetachSession => Err(PortError::new(
                "COMMAND_NOT_EXECUTABLE",
                "command is not an action",
                false,
            )),
        }
    }
}

fn active_window_address(output: &[u8]) -> PortResult<String> {
    let value: serde_json::Value = serde_json::from_slice(output).map_err(|error| {
        PortError::new(
            "HYPRLAND_UNAVAILABLE",
            format!("unable to parse active window: {error}"),
            false,
        )
    })?;
    let address = value
        .get("address")
        .and_then(serde_json::Value::as_str)
        .filter(|address| is_window_address(address))
        .ok_or_else(|| {
            PortError::new(
                "HYPRLAND_TARGET_MISSING",
                "Hyprland has no valid active window",
                false,
            )
        })?;

    Ok(address.to_owned())
}

fn is_window_address(address: &str) -> bool {
    address.strip_prefix("0x").is_some_and(|hex| {
        !hex.is_empty() && hex.chars().all(|character| character.is_ascii_hexdigit())
    })
}

fn modifier_key(modifier: KeyModifier) -> &'static str {
    match modifier {
        KeyModifier::Super => "Super_L",
        KeyModifier::Ctrl => "Control_L",
        KeyModifier::Alt => "Alt_L",
        KeyModifier::Shift => "Shift_L",
    }
}

fn key_state_dispatcher(key: &str, state: &str) -> String {
    format!("hl.dsp.send_key_state({{ mods = \"\", key = \"{key}\", state = \"{state}\" }})")
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

fn command_dispatcher(command: &RemoteCommand) -> Option<String> {
    match command {
        RemoteCommand::SendShortcut { chord, window } => Some(format!(
            "hl.dsp.send_shortcut({{ mods = \"{}\", key = \"{}\", window = \"{}\" }})",
            chord.modifiers_hypr(),
            chord.key(),
            window.as_hypr()
        )),
        RemoteCommand::CloseWindow { window } => Some(format!(
            "hl.dsp.window.close({{ window = \"{}\" }})",
            window.as_hypr()
        )),
        RemoteCommand::SwitchStreamDisplay { .. }
        | RemoteCommand::AttachSession { .. }
        | RemoteCommand::DetachSession => None,
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
        RemoteCommand::SwitchStreamDisplay { .. }
        | RemoteCommand::AttachSession { .. }
        | RemoteCommand::DetachSession => None,
    }
}

fn notify(message: &str) -> String {
    format!(
        "hl.dispatch(hl.dsp.exec_cmd(\"omarchy-notification-send 'Omarchy Desk' '{message}'\"))"
    )
}

fn controller_action_lua(bind: &Keybind) -> Option<String> {
    let dispatcher = command_dispatcher(&bind.command)?;
    let notify = notify(bind.description);

    Some(format!(
        "function() hl.dispatch({dispatcher}); {notify} end"
    ))
}

fn remote_action_lua(bind: &Keybind, controller: &AgentEndpoint) -> Option<String> {
    let invocation = command_cli_invocation(&bind.command, controller)?;
    let shell = format!(
        "log=\\\"${{XDG_RUNTIME_DIR:-/tmp}}/omdesk-keybind.log\\\"; \
{invocation} >\\\"$log\\\" 2>&1 || \
omarchy-notification-send 'Omarchy Desk' \\\"Omarchy Desk failed: $(cat \\\"$log\\\")\\\""
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
{{ allow_input_capture = {capture}, description = \"Omarchy Desk: {description}\" }}))",
            trigger = bind.trigger,
            description = bind.description,
            capture = role == SessionRole::Remote && bind.allow_input_capture,
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
    use omdesk_application::ports::{ChildProcess, CommandOutput};
    use std::{collections::VecDeque, sync::Mutex};

    struct ScriptedRunner {
        calls: Mutex<Vec<CommandSpec>>,
        results: Mutex<VecDeque<PortResult<CommandOutput>>>,
    }

    impl ScriptedRunner {
        fn new(results: impl IntoIterator<Item = PortResult<CommandOutput>>) -> Self {
            Self {
                calls: Mutex::new(Vec::new()),
                results: Mutex::new(results.into_iter().collect()),
            }
        }

        fn calls(&self) -> Vec<CommandSpec> {
            self.calls.lock().expect("calls lock").clone()
        }
    }

    #[async_trait]
    impl CommandRunner for ScriptedRunner {
        async fn run(&self, spec: CommandSpec) -> PortResult<CommandOutput> {
            self.calls.lock().expect("calls lock").push(spec);
            self.results
                .lock()
                .expect("results lock")
                .pop_front()
                .unwrap_or_else(|| Ok(output("")))
        }

        async fn spawn(&self, _spec: CommandSpec) -> PortResult<Box<dyn ChildProcess>> {
            Err(PortError::new(
                "UNEXPECTED_SPAWN",
                "spawn is not used",
                false,
            ))
        }
    }

    fn output(stdout: &str) -> CommandOutput {
        CommandOutput {
            status: 0,
            stdout: stdout.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn failure(message: &str) -> PortResult<CommandOutput> {
        Err(PortError::new("TEST_FAILURE", message, false))
    }

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
        assert!(spec.args[1].contains("omarchy-notification-send 'Omarchy Desk'"));
        assert!(spec.args[1].contains("allow_input_capture = false"));
        assert!(!spec.args[1].contains("allow_input_capture = true"));
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
        assert!(!spec.args[1].contains("&& omarchy-notification-send 'Omarchy Desk'"));
        assert!(spec.args[1].contains("|| omarchy-notification-send 'Omarchy Desk'"));
        assert!(spec.args[1].contains("allow_input_capture = true"));
        assert!(!spec.args[1].contains("allow_input_capture = false"));
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
    fn test_remove_clears_only_omdesk_bindings() {
        let spec = remove_session_keybinds_spec();

        assert!(spec.args[1].contains("omdesk_binds"));
        assert!(spec.args[1].contains(":remove()"));
        assert!(!spec.args[1].contains("hl.unbind"));
    }

    #[test]
    fn test_command_dispatcher_uses_validated_tokens() {
        let dispatcher = command_dispatcher(&moonlight_shortcut("Z")).expect("action renders");

        assert_eq!(
            dispatcher,
            "hl.dsp.send_shortcut({ mods = \"CTRL ALT SHIFT\", key = \"Z\", \
window = \"class:com.moonlight_stream.Moonlight\" })"
        );
    }

    #[tokio::test]
    async fn test_shortcut_focuses_target_sends_key_states_and_restores_focus() {
        let runner = Arc::new(ScriptedRunner::new([Ok(output(
            r#"{"address":"0xabc123"}"#,
        ))]));
        let executor = HyprlandCommandExecutor::new(runner.clone());

        executor
            .execute(moonlight_shortcut("F2"))
            .await
            .expect("shortcut executes");

        let calls = runner.calls();
        assert_eq!(calls.len(), 11);
        assert_eq!(calls[0].args, ["activewindow", "-j"]);
        assert_eq!(
            calls[1].args[1],
            "hl.dispatch(hl.dsp.focus({ window = \"class:com.moonlight_stream.Moonlight\" }))"
        );
        assert!(calls[2].args[1].contains("key = \"Control_L\", state = \"down\""));
        assert!(calls[3].args[1].contains("key = \"Alt_L\", state = \"down\""));
        assert!(calls[4].args[1].contains("key = \"Shift_L\", state = \"down\""));
        assert!(calls[5].args[1].contains("key = \"F2\", state = \"down\""));
        assert!(calls[6].args[1].contains("key = \"F2\", state = \"up\""));
        assert!(calls[7].args[1].contains("key = \"Shift_L\", state = \"up\""));
        assert!(calls[8].args[1].contains("key = \"Alt_L\", state = \"up\""));
        assert!(calls[9].args[1].contains("key = \"Control_L\", state = \"up\""));
        assert_eq!(calls[9].args[0], "eval");
        assert!(calls.last().expect("restore call").args[1].contains("address:0xabc123"));
    }

    #[tokio::test]
    async fn test_shortcut_releases_pressed_keys_and_restores_focus_after_failure() {
        let runner = Arc::new(ScriptedRunner::new([
            Ok(output(r#"{"address":"0xabc123"}"#)),
            Ok(output("")),
            Ok(output("")),
            failure("alt down failed"),
        ]));
        let executor = HyprlandCommandExecutor::new(runner.clone());

        let error = executor
            .execute(moonlight_shortcut("F2"))
            .await
            .expect_err("shortcut fails");

        assert_eq!(error.message, "alt down failed");
        let calls = runner.calls();
        assert_eq!(calls.len(), 7);
        assert!(calls[4].args[1].contains("key = \"Alt_L\", state = \"up\""));
        assert!(calls[5].args[1].contains("key = \"Control_L\", state = \"up\""));
        assert!(calls[6].args[1].contains("address:0xabc123"));
    }

    #[tokio::test]
    async fn test_shortcut_rejects_invalid_previous_window_without_injecting_input() {
        let runner = Arc::new(ScriptedRunner::new([Ok(output(
            r#"{"address":"$(unsafe)"}"#,
        ))]));
        let executor = HyprlandCommandExecutor::new(runner.clone());

        let error = executor
            .execute(moonlight_shortcut("F2"))
            .await
            .expect_err("invalid address fails");

        assert_eq!(error.code, "HYPRLAND_TARGET_MISSING");
        assert_eq!(runner.calls().len(), 1);
    }

    #[tokio::test]
    async fn test_active_window_shortcut_uses_key_state_flow_without_focus_changes() {
        let runner = Arc::new(ScriptedRunner::new([]));
        let executor = HyprlandCommandExecutor::new(runner.clone());
        let command = RemoteCommand::SendShortcut {
            chord: KeyChord::new([KeyModifier::Ctrl], "Z").expect("valid chord"),
            window: WindowSelector::ActiveWindow,
        };

        executor.execute(command).await.expect("shortcut executes");

        let calls = runner.calls();
        assert_eq!(calls.len(), 4);
        assert!(calls[0].args[1].contains("Control_L"));
        assert!(calls[1].args[1].contains("key = \"Z\", state = \"down\""));
        assert!(calls[2].args[1].contains("key = \"Z\", state = \"up\""));
        assert!(calls[3].args[1].contains("Control_L"));
        assert!(
            calls
                .iter()
                .all(|call| !call.args[1].contains("hl.dsp.focus"))
        );
    }
}
