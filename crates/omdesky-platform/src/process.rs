use async_trait::async_trait;
use omdesky_application::ports::{
    CapturePolicy, ChildProcess, CommandOutput, CommandRunner, CommandSpec, PortError, PortResult,
    StdinPolicy,
};
use std::{process::Stdio, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::Command,
    time::timeout,
};

#[derive(Clone, Debug, Default)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, spec: CommandSpec) -> PortResult<CommandOutput> {
        let duration = spec.timeout;
        let mut child = build_command(&spec)
            .spawn()
            .map_err(|error| spawn_error(&spec.program, error))?;
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        let (sender, mut receiver) = tokio::sync::mpsc::channel(2);
        let stdout_sender = sender.clone();
        tokio::spawn(async move {
            let _ = stdout_sender
                .send((true, read_limited(stdout, spec.stdout_limit).await))
                .await;
        });
        tokio::spawn(async move {
            let _ = sender
                .send((false, read_limited(stderr, spec.stderr_limit).await))
                .await;
        });
        let execution = async {
            let mut status = None;
            let mut stdout = None;
            let mut stderr = None;

            while status.is_none() || stdout.is_none() || stderr.is_none() {
                tokio::select! {
                    result = child.wait(), if status.is_none() => {
                        status = Some(result.map_err(|error| spawn_error(&spec.program, error))?);
                    }
                    result = receiver.recv(), if stdout.is_none() || stderr.is_none() => {
                        let (is_stdout, output) = result.ok_or_else(|| {
                            PortError::new(
                                "COMMAND_OUTPUT_FAILED",
                                "command output reader stopped unexpectedly",
                                false,
                            )
                        })?;
                        let output = match output {
                            Ok(output) => output,
                            Err(error) => {
                                child
                                    .kill()
                                    .await
                                    .map_err(|kill_error| terminate_error(&spec.program, kill_error))?;

                                return Err(error);
                            }
                        };

                        if is_stdout {
                            stdout = Some(output);
                        } else {
                            stderr = Some(output);
                        }
                    }
                }
            }

            Ok::<_, PortError>((
                status.expect("status is populated"),
                stdout.expect("stdout is populated"),
                stderr.expect("stderr is populated"),
            ))
        };

        let result = timeout(duration, execution).await;
        let (status, stdout, stderr) = match result {
            Ok(result) => result?,
            Err(_) => {
                child
                    .kill()
                    .await
                    .map_err(|error| terminate_error(&spec.program, error))?;

                return Err(timeout_error(&spec.program, duration));
            }
        };
        let status_code = status.code().unwrap_or(-1);

        if !status.success() {
            return Err(PortError::new(
                "COMMAND_FAILED",
                format!("{} exited with status {status_code}", spec.program),
                false,
            ));
        }

        Ok(CommandOutput {
            status: status_code,
            stdout,
            stderr,
        })
    }

    async fn spawn(&self, spec: CommandSpec) -> PortResult<Box<dyn ChildProcess>> {
        let child = build_command(&spec)
            .spawn()
            .map_err(|error| spawn_error(&spec.program, error))?;
        Ok(Box::new(TokioChildProcess { child }))
    }
}

fn build_command(spec: &CommandSpec) -> Command {
    let mut command = Command::new(&spec.program);
    command.args(&spec.args);

    if spec.clear_environment {
        command.env_clear();
    }

    command.envs(&spec.environment);

    if let Some(directory) = &spec.working_directory {
        command.current_dir(directory);
    }

    command.stdin(match spec.stdin {
        StdinPolicy::Null => Stdio::null(),
        StdinPolicy::Inherit => Stdio::inherit(),
    });

    match spec.capture {
        CapturePolicy::Both => {
            command.stdout(Stdio::piped()).stderr(Stdio::piped());
        }
        CapturePolicy::Inherit => {
            command.stdout(Stdio::inherit()).stderr(Stdio::inherit());
        }
        CapturePolicy::Discard => {
            command.stdout(Stdio::null()).stderr(Stdio::null());
        }
    }

    command.kill_on_drop(true);
    command
}

async fn read_limited(stream: Option<impl AsyncRead + Unpin>, limit: usize) -> PortResult<Vec<u8>> {
    let Some(mut stream) = stream else {
        return Ok(Vec::new());
    };
    let mut output = Vec::new();
    let mut buffer = [0_u8; 8192];

    loop {
        let count = stream
            .read(&mut buffer)
            .await
            .map_err(|error| PortError::new("COMMAND_OUTPUT_FAILED", error.to_string(), false))?;

        if count == 0 {
            return Ok(output);
        }

        if output.len().saturating_add(count) > limit {
            return Err(PortError::new(
                "COMMAND_OUTPUT_LIMIT",
                format!("command output exceeded {limit} bytes"),
                false,
            ));
        }

        output.extend_from_slice(&buffer[..count]);
    }
}

fn timeout_error(program: &str, duration: Duration) -> PortError {
    PortError::new(
        "COMMAND_TIMEOUT",
        format!(
            "{program} exceeded timeout of {} seconds",
            duration.as_secs()
        ),
        true,
    )
}

fn terminate_error(program: &str, error: std::io::Error) -> PortError {
    PortError::new(
        "PROCESS_TERMINATE_FAILED",
        format!("unable to terminate {program}: {error}"),
        false,
    )
}

fn spawn_error(program: &str, error: std::io::Error) -> PortError {
    PortError::new(
        "COMMAND_NOT_AVAILABLE",
        format!("unable to execute {program}: {error}"),
        false,
    )
}

struct TokioChildProcess {
    child: tokio::process::Child,
}

#[async_trait]
impl ChildProcess for TokioChildProcess {
    fn id(&self) -> Option<u32> {
        self.child.id()
    }

    async fn wait(&mut self) -> PortResult<i32> {
        self.child
            .wait()
            .await
            .map(|status| status.code().unwrap_or(-1))
            .map_err(|error| PortError::new("PROCESS_WAIT_FAILED", error.to_string(), false))
    }

    async fn terminate(&mut self) -> PortResult<()> {
        self.child
            .kill()
            .await
            .map_err(|error| PortError::new("PROCESS_TERMINATE_FAILED", error.to_string(), false))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_runner_passes_arguments_without_shell_interpretation() {
        let runner = TokioCommandRunner;
        let spec = CommandSpec::new(
            "/usr/bin/printf",
            ["%s".to_owned(), "; echo unsafe".to_owned()],
        );
        let output = runner.run(spec).await.expect("command succeeds");

        assert_eq!(output.stdout, b"; echo unsafe");
    }

    #[tokio::test]
    async fn test_runner_rejects_nonzero_exit() {
        let runner = TokioCommandRunner;
        let error = runner
            .run(CommandSpec::new("/usr/bin/false", []))
            .await
            .expect_err("nonzero exit fails");

        assert_eq!(error.code, "COMMAND_FAILED");
    }

    #[tokio::test]
    async fn test_runner_rejects_stdout_over_limit() {
        let runner = TokioCommandRunner;
        let mut spec =
            CommandSpec::new("/usr/bin/printf", ["%s".to_owned(), "123456789".to_owned()]);
        spec.stdout_limit = 8;

        let error = runner.run(spec).await.expect_err("large output fails");

        assert_eq!(error.code, "COMMAND_OUTPUT_LIMIT");
    }

    #[tokio::test]
    async fn test_runner_terminates_flooding_process_immediately() {
        let runner = TokioCommandRunner;
        let mut spec = CommandSpec::new("/usr/bin/yes", []);
        spec.stdout_limit = 8;
        spec.timeout = Duration::from_secs(5);
        let started = std::time::Instant::now();

        let error = runner.run(spec).await.expect_err("flooding output fails");

        assert_eq!(error.code, "COMMAND_OUTPUT_LIMIT");
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[tokio::test]
    async fn test_runner_rejects_stderr_over_limit() {
        let runner = TokioCommandRunner;
        let mut spec = CommandSpec::new(
            "/usr/bin/sh",
            ["-c".to_owned(), "printf 123456789 >&2".to_owned()],
        );
        spec.stderr_limit = 8;

        let error = runner
            .run(spec)
            .await
            .expect_err("large error output fails");

        assert_eq!(error.code, "COMMAND_OUTPUT_LIMIT");
    }

    #[tokio::test]
    async fn test_runner_can_clear_inherited_environment() {
        let runner = TokioCommandRunner;
        let mut spec = CommandSpec::new("/usr/bin/env", []);
        spec.clear_environment = true;
        spec.environment.insert("LANG".to_owned(), "C".to_owned());

        let output = runner.run(spec).await.expect("command succeeds");

        assert_eq!(output.stdout, b"LANG=C\n");
    }
}
