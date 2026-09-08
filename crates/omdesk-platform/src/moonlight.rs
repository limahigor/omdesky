use async_trait::async_trait;
use omdesk_application::ports::{
    CapturePolicy, ChildProcess, CommandRunner, CommandSpec, PairingState, PendingPairing,
    PortError, PortResult, StreamClient, StreamHostDescriptor, StreamLaunchRequest,
};
use omdesk_core::CodecPreference;
use std::{process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
    time::timeout,
};

const PAIRING_TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct MoonlightAdapter {
    runner: Arc<dyn CommandRunner>,
}

impl MoonlightAdapter {
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    pub async fn version(&self) -> PortResult<String> {
        let output = self
            .runner
            .run(CommandSpec::new("moonlight", ["--version".to_owned()]))
            .await
            .map_err(|error| PortError::new("MOONLIGHT_NOT_INSTALLED", error.message, false))?;
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
    }
}

#[async_trait]
impl StreamClient for MoonlightAdapter {
    async fn pairing_state(&self, host: &StreamHostDescriptor) -> PortResult<PairingState> {
        let output = self
            .runner
            .run(CommandSpec::new(
                "moonlight",
                ["list".to_owned(), host.address.to_string()],
            ))
            .await;

        match output {
            Ok(_) => Ok(PairingState::Paired),
            Err(error) if error.code == "COMMAND_FAILED" => Ok(PairingState::Required),
            Err(error) if error.code == "COMMAND_NOT_AVAILABLE" => Err(PortError::new(
                "MOONLIGHT_NOT_INSTALLED",
                error.message,
                false,
            )),
            Err(error) => Err(PortError::new(
                "MOONLIGHT_UNSUPPORTED",
                error.message,
                false,
            )),
        }
    }

    async fn begin_pairing(&self, host: &StreamHostDescriptor) -> PortResult<PendingPairing> {
        let mut child = Command::new("moonlight")
            .arg("pair")
            .arg(host.address.to_string())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            .kill_on_drop(false)
            .spawn()
            .map_err(|error| {
                PortError::new(
                    "MOONLIGHT_NOT_INSTALLED",
                    format!("unable to launch Moonlight pairing: {error}"),
                    false,
                )
            })?;

        let stdout = child.stdout.take().ok_or_else(|| {
            PortError::new(
                "MOONLIGHT_PAIRING_FAILED",
                "Moonlight did not expose its pairing output",
                false,
            )
        })?;

        let pin = timeout(PAIRING_TIMEOUT, read_pin(stdout))
            .await
            .map_err(|_| {
                PortError::new(
                    "MOONLIGHT_PAIRING_FAILED",
                    "timed out waiting for Moonlight to produce a pairing PIN",
                    true,
                )
            })??;

        tokio::spawn(async move {
            let _ = child.wait().await;
        });

        Ok(PendingPairing { pin })
    }

    async fn launch(&self, request: StreamLaunchRequest) -> PortResult<Box<dyn ChildProcess>> {
        self.runner
            .spawn(launch_spec(&request))
            .await
            .map_err(|error| PortError::new("STREAM_START_FAILED", error.message, error.retryable))
    }
}

fn launch_spec(request: &StreamLaunchRequest) -> CommandSpec {
    let mut spec = CommandSpec::new("moonlight", launch_arguments(request));
    spec.timeout = Duration::from_secs(30);
    spec.capture = CapturePolicy::Discard;
    spec
}

async fn read_pin(stdout: tokio::process::ChildStdout) -> PortResult<String> {
    let mut lines = BufReader::new(stdout).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        if let Some(pin) = extract_pin(&line) {
            return Ok(pin);
        }
    }
    Err(PortError::new(
        "MOONLIGHT_PAIRING_FAILED",
        "Moonlight exited without printing a pairing PIN",
        true,
    ))
}

/// Extract the first 4-digit PIN from a Moonlight pairing line such as
/// "Please enter the following PIN on the host PC: 1234".
pub fn extract_pin(line: &str) -> Option<String> {
    let digits: String = line
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit())
        .collect();
    (digits.len() == 4).then_some(digits)
}

/// Build the argument list for the modern moonlight-qt CLI, which uses
/// `--flag value` syntax (e.g. `--resolution`, `--video-codec`,
/// `--capture-system-keys`) rather than the legacy `-flag=value` form. We only
/// emit non-default flags to keep the invocation resilient across versions.
pub fn launch_arguments(request: &StreamLaunchRequest) -> Vec<String> {
    let mut args = vec![
        "stream".to_owned(),
        request.host.address.to_string(),
        request.host.application.clone(),
        "--resolution".to_owned(),
        format!("{}x{}", request.profile.width, request.profile.height),
        "--fps".to_owned(),
        request.profile.fps.to_string(),
    ];

    // Cap the bitrate when requested (Moonlight expects kilobits per second).
    if let Some(bitrate_kbps) = request.profile.bitrate_kbps {
        args.push("--bitrate".to_owned());
        args.push(bitrate_kbps.to_string());
    }

    if !request.fullscreen {
        args.push("--display-mode".to_owned());
        args.push("windowed".to_owned());
    }

    match request.profile.codec_preference {
        CodecPreference::Auto => {}
        CodecPreference::H264 => {
            args.push("--video-codec".to_owned());
            args.push("H.264".to_owned());
        }
        CodecPreference::Hevc => {
            args.push("--video-codec".to_owned());
            args.push("HEVC".to_owned());
        }
        CodecPreference::Av1 => {
            args.push("--video-codec".to_owned());
            args.push("AV1".to_owned());
        }
    }

    if request.input_mode == omdesk_core::InputMode::Remote {
        args.push("--capture-system-keys".to_owned());
        args.push("always".to_owned());
    }

    args
}

#[cfg(test)]
mod tests {
    use super::*;
    use omdesk_core::{CodecPreference, InputMode, StreamProfile};
    use std::net::{IpAddr, Ipv4Addr};

    fn request(fullscreen: bool, input_mode: InputMode) -> StreamLaunchRequest {
        StreamLaunchRequest {
            host: StreamHostDescriptor {
                address: IpAddr::V4(Ipv4Addr::new(100, 64, 0, 2)),
                application: "Desktop".to_owned(),
            },
            profile: StreamProfile::default(),
            fullscreen,
            input_mode,
        }
    }

    #[test]
    fn test_launch_arguments_use_modern_moonlight_cli() {
        assert_eq!(
            launch_arguments(&request(false, InputMode::Local)),
            [
                "stream",
                "100.64.0.2",
                "Desktop",
                "--resolution",
                "1920x1080",
                "--fps",
                "60",
                "--display-mode",
                "windowed",
            ]
        );
    }

    #[test]
    fn test_stream_output_is_discarded_to_protect_terminal_ui() {
        let spec = launch_spec(&request(true, InputMode::Local));

        assert_eq!(spec.capture, CapturePolicy::Discard);
    }

    #[test]
    fn test_fullscreen_omits_display_mode_override() {
        let args = launch_arguments(&request(true, InputMode::Local));
        assert!(!args.contains(&"--display-mode".to_owned()));
        assert!(args.contains(&"--resolution".to_owned()));
    }

    #[test]
    fn test_specific_codec_uses_moonlight_codec_names() {
        let mut request = request(true, InputMode::Local);
        request.profile.codec_preference = CodecPreference::H264;
        let args = launch_arguments(&request);
        let index = args
            .iter()
            .position(|a| a == "--video-codec")
            .expect("codec");
        assert_eq!(args[index + 1], "H.264");
    }

    #[test]
    fn test_remote_input_enables_system_key_capture() {
        let args = launch_arguments(&request(false, InputMode::Remote));
        let index = args
            .iter()
            .position(|a| a == "--capture-system-keys")
            .expect("capture flag present");
        assert_eq!(args[index + 1], "always");
    }

    #[test]
    fn test_fullscreen_remote_stream_captures_system_keys() {
        let args = launch_arguments(&request(true, InputMode::Remote));
        let index = args
            .iter()
            .position(|argument| argument == "--capture-system-keys")
            .expect("capture flag present");

        assert_eq!(args[index + 1], "always");
    }

    #[test]
    fn test_bitrate_cap_is_passed_in_kbps() {
        let mut request = request(true, InputMode::Local);
        request.profile.bitrate_kbps = Some(10_000);
        let args = launch_arguments(&request);
        let index = args.iter().position(|a| a == "--bitrate").expect("bitrate");
        assert_eq!(args[index + 1], "10000");
    }

    #[test]
    fn test_extract_pin_reads_four_digit_code() {
        assert_eq!(
            extract_pin("Please enter the following PIN on the host PC: 1234"),
            Some("1234".to_owned())
        );
        assert_eq!(extract_pin("no pin here"), None);
    }
}
