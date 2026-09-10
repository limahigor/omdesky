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
        tracing::debug!(program = %spec.program, args = ?spec.args, "hyprland.command.start");

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

    async fn eval_batch(&self, dispatchers: Vec<String>) -> PortResult<()> {
        self.run_hyprland(CommandSpec::new(
            "hyprctl",
            ["--batch".to_owned(), hyprctl_batch_argument(&dispatchers)],
        ))
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
        let lua = timed_shortcut_lua(chord, window, previous.as_deref());

        tracing::info!(
            mods = %chord.modifiers_hypr(),
            key = %chord.key(),
            window = %window.as_hypr(),
            previous = ?previous,
            "hyprland.shortcut.dispatch"
        );

        self.run_hyprland(CommandSpec::new("hyprctl", ["eval".to_owned(), lua]))
            .await
            .map(|_| ())
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
                let actions = command_actions(&command, &[]).ok_or_else(|| {
                    PortError::new("COMMAND_NOT_EXECUTABLE", "command is not an action", false)
                })?;
                self.eval_batch(actions).await
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

fn focus_dispatcher(window: &WindowSelector) -> String {
    format!("hl.dsp.focus({{ window = \"{}\" }})", window.as_hypr())
}

fn restore_focus_dispatcher(address: &str) -> String {
    format!("hl.dsp.focus({{ window = \"address:{address}\" }})")
}

fn timed_shortcut_lua(
    chord: &KeyChord,
    window: &WindowSelector,
    restore_address: Option<&str>,
) -> String {
    let focus = match window {
        WindowSelector::ActiveWindow => String::new(),
        WindowSelector::Class(_) => format!("hl.dispatch({}); ", focus_dispatcher(window)),
    };

    if chord.key() == "Z" {
        let restore = restore_address.map_or_else(String::new, |address| {
            format!("; hl.dispatch({})", restore_focus_dispatcher(address))
        });

        return format!(
            "{focus}hl.dispatch(hl.dsp.send_key_state({{ mods = \"{mods}\", key = \"{key}\", state = \"down\" }})); \
hl.timer(function() hl.dispatch(hl.dsp.send_key_state({{ mods = \"{mods}\", key = \"{key}\", state = \"up\" }})){restore} end, \
{{ timeout = 50, type = \"oneshot\" }}); return true",
            mods = chord.modifiers_hypr(),
            key = chord.key(),
        );
    }

    let mut actions = shortcut_key_dispatchers(chord);
    if let Some(address) = restore_address {
        actions.push(restore_focus_dispatcher(address));
    }
    let last = actions.pop().expect("validated shortcut has key actions");
    let mut sequence = format!("hl.dispatch({last}); return true");

    for action in actions.into_iter().rev() {
        sequence = format!(
            "hl.dispatch({action}); hl.timer(function() {sequence} end, \
{{ timeout = 25, type = \"oneshot\" }})"
        );
    }

    format!("{focus}{sequence}")
}

fn shortcut_key_dispatchers(chord: &KeyChord) -> Vec<String> {
    let mut dispatchers = chord
        .modifiers()
        .iter()
        .map(|modifier| key_state_dispatcher(modifier_key(*modifier), "down"))
        .collect::<Vec<_>>();
    dispatchers.push(key_state_dispatcher(chord.key(), "down"));
    dispatchers.push(key_state_dispatcher(chord.key(), "up"));
    dispatchers.extend(
        chord
            .modifiers()
            .iter()
            .rev()
            .map(|modifier| key_state_dispatcher(modifier_key(*modifier), "up")),
    );

    dispatchers
}

fn hyprctl_batch_argument(dispatchers: &[String]) -> String {
    dispatchers
        .iter()
        .map(|dispatcher| format!("eval hl.dispatch({dispatcher})"))
        .collect::<Vec<_>>()
        .join(" ; ")
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

fn command_actions(command: &RemoteCommand, held_modifiers: &[KeyModifier]) -> Option<Vec<String>> {
    match command {
        RemoteCommand::SendShortcut { chord, window } => {
            let mut actions = vec![focus_dispatcher(window)];
            actions.extend(
                held_modifiers
                    .iter()
                    .filter(|modifier| !chord.modifiers().contains(modifier))
                    .map(|modifier| key_state_dispatcher(modifier_key(*modifier), "up")),
            );
            actions.extend(shortcut_key_dispatchers(chord));
            Some(actions)
        }
        RemoteCommand::CloseWindow { window } => Some(vec![format!(
            "hl.dsp.window.close({{ window = \"{}\" }})",
            window.as_hypr()
        )]),
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
    let action = match &bind.command {
        RemoteCommand::SendShortcut { chord, window } => format!(
            "function() hl.timer(function() hl.dispatch({focus}); \
hl.dispatch(hl.dsp.send_key_state({{ mods = \"{mods}\", key = \"{key}\", state = \"down\" }})); \
hl.timer(function() hl.dispatch(hl.dsp.send_key_state({{ mods = \"{mods}\", key = \"{key}\", state = \"up\" }})) end, \
{{ timeout = 50, type = \"oneshot\" }}) end, {{ timeout = 100, type = \"oneshot\" }}) end",
            focus = focus_dispatcher(window),
            mods = chord.modifiers_hypr(),
            key = chord.key(),
        ),
        RemoteCommand::CloseWindow { .. } => {
            let actions = command_actions(&bind.command, &[])?;
            let batch = hyprctl_batch_argument(&actions);
            format!(
                "function() hl.dispatch(hl.dsp.exec_cmd([==[hyprctl --batch '{batch}']==])) end"
            )
        }
        RemoteCommand::SwitchStreamDisplay { .. }
        | RemoteCommand::AttachSession { .. }
        | RemoteCommand::DetachSession => return None,
    };
    let notify = notify(bind.description);

    Some(format!("function() ({action})(); {notify} end"))
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
        assert!(spec.args[1].contains("hl.timer(function()"));
        assert!(spec.args[1].contains("timeout = 100"));
        assert!(
            spec.args[1]
                .contains("hl.dsp.focus({ window = \"class:com.moonlight_stream.Moonlight\" })")
        );
        assert!(spec.args[1].contains(
            "hl.dsp.send_key_state({ mods = \"CTRL ALT SHIFT\", key = \"Z\", state = \"down\" })"
        ));
        assert!(spec.args[1].contains(
            "hl.dsp.send_key_state({ mods = \"CTRL ALT SHIFT\", key = \"Z\", state = \"up\" })"
        ));
        assert!(spec.args[1].contains("timeout = 50"));
        assert!(!spec.args[1].contains("key = \"Super_L\""));
        assert!(!spec.args[1].contains("hl.dsp.send_shortcut"));
        assert!(!spec.args[1].contains("omdesk command"));
        assert!(spec.args[1].contains("omarchy-notification-send 'Omarchy Desk'"));
        assert!(spec.args[1].contains("allow_input_capture = false"));
        assert!(!spec.args[1].contains("allow_input_capture = true"));
    }

    #[test]
    fn test_close_session_bind_closes_moonlight_window() {
        let controller_spec = install_session_keybinds_spec(SessionRole::Controller, None);
        assert!(controller_spec.args[1].contains(
            "eval hl.dispatch(hl.dsp.window.close({ window = \"class:com.moonlight_stream.Moonlight\" }))"
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
    fn test_command_actions_build_focus_and_key_state_sequence() {
        let actions = command_actions(&moonlight_shortcut("Z"), &[]).expect("action renders");

        assert_eq!(
            actions,
            vec![
                "hl.dsp.focus({ window = \"class:com.moonlight_stream.Moonlight\" })".to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Control_L\", state = \"down\" })"
                    .to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Alt_L\", state = \"down\" })"
                    .to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Shift_L\", state = \"down\" })"
                    .to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Z\", state = \"down\" })".to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Z\", state = \"up\" })".to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Shift_L\", state = \"up\" })"
                    .to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Alt_L\", state = \"up\" })"
                    .to_owned(),
                "hl.dsp.send_key_state({ mods = \"\", key = \"Control_L\", state = \"up\" })"
                    .to_owned(),
            ]
        );
    }

    #[test]
    fn test_command_actions_release_held_trigger_modifiers_before_chord() {
        let actions = command_actions(&moonlight_shortcut("Z"), &[KeyModifier::Super])
            .expect("action renders");

        assert_eq!(
            actions[0],
            "hl.dsp.focus({ window = \"class:com.moonlight_stream.Moonlight\" })"
        );
        assert_eq!(
            actions[1],
            "hl.dsp.send_key_state({ mods = \"\", key = \"Super_L\", state = \"up\" })"
        );
        assert_eq!(
            actions[2],
            "hl.dsp.send_key_state({ mods = \"\", key = \"Control_L\", state = \"down\" })"
        );
        assert!(
            !actions
                .iter()
                .any(|action| action.contains("key = \"Super_L\", state = \"down\""))
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
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].args, ["activewindow", "-j"]);
        assert_eq!(calls[1].args[0], "eval");
        let lua = &calls[1].args[1];
        assert!(
            lua.contains("hl.dsp.focus({ window = \"class:com.moonlight_stream.Moonlight\" })")
        );
        let expected = [
            "key = \"Control_L\", state = \"down\"",
            "key = \"Alt_L\", state = \"down\"",
            "key = \"Shift_L\", state = \"down\"",
            "key = \"F2\", state = \"down\"",
            "key = \"F2\", state = \"up\"",
            "key = \"Shift_L\", state = \"up\"",
            "key = \"Alt_L\", state = \"up\"",
            "key = \"Control_L\", state = \"up\"",
            "address:0xabc123",
        ];
        let mut previous = 0;
        for fragment in expected {
            let position = lua[previous..]
                .find(fragment)
                .map(|position| position + previous)
                .expect("timed sequence contains ordered action");
            previous = position + fragment.len();
        }
        assert_eq!(lua.matches("timeout = 25").count(), 8);
        assert!(!lua.contains("mods = \"CTRL ALT SHIFT\""));
    }

    #[tokio::test]
    async fn test_capture_toggle_uses_combined_modifiers_for_moonlight_hotkey() {
        let runner = Arc::new(ScriptedRunner::new([Ok(output(
            r#"{"address":"0xabc123"}"#,
        ))]));
        let executor = HyprlandCommandExecutor::new(runner.clone());

        executor
            .execute(moonlight_shortcut("Z"))
            .await
            .expect("shortcut executes");

        let calls = runner.calls();
        let lua = &calls[1].args[1];
        assert!(lua.contains(
            "hl.dsp.send_key_state({ mods = \"CTRL ALT SHIFT\", key = \"Z\", state = \"down\" })"
        ));
        assert!(lua.contains(
            "hl.dsp.send_key_state({ mods = \"CTRL ALT SHIFT\", key = \"Z\", state = \"up\" })"
        ));
        assert_eq!(lua.matches("timeout = 50").count(), 1);
    }

    #[tokio::test]
    async fn test_shortcut_restores_focus_when_target_focus_fails() {
        let runner = Arc::new(ScriptedRunner::new([
            Ok(output(r#"{"address":"0xabc123"}"#)),
            failure("target focus failed"),
        ]));
        let executor = HyprlandCommandExecutor::new(runner.clone());

        let error = executor
            .execute(moonlight_shortcut("F2"))
            .await
            .expect_err("shortcut fails");

        assert_eq!(error.message, "target focus failed");
        let calls = runner.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[1].args[1].contains("address:0xabc123"));
        assert!(calls[1].args[1].contains("send_key_state"));
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
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].args[0], "eval");
        let lua = &calls[0].args[1];
        assert!(
            lua.contains(
                "hl.dsp.send_key_state({ mods = \"CTRL\", key = \"Z\", state = \"down\" })"
            )
        );
        assert!(
            lua.contains("hl.dsp.send_key_state({ mods = \"CTRL\", key = \"Z\", state = \"up\" })")
        );
        assert!(!lua.contains("hl.dsp.focus"));
    }
}
