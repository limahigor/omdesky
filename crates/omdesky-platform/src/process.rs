use async_trait::async_trait;
use omdesky_application::ports::{
    CapturePolicy, ChildProcess, CommandOutput, CommandRunner, CommandSpec, PortError, PortResult,
    StdinPolicy,
};
use std::{process::Stdio, time::Duration};
use tokio::{process::Command, time::timeout};

#[derive(Clone, Debug, Default)]
pub struct TokioCommandRunner;

#[async_trait]
impl CommandRunner for TokioCommandRunner {
    async fn run(&self, spec: CommandSpec) -> PortResult<CommandOutput> {
        let duration = spec.timeout;
        let mut command = build_command(&spec);

        let output = timeout(duration, command.output())
            .await
            .map_err(|_| timeout_error(&spec.program, duration))?
            .map_err(|error| spawn_error(&spec.program, error))?;

        let status = output.status.code().unwrap_or(-1);

        if !output.status.success() {
            return Err(PortError::new(
                "COMMAND_FAILED",
                format!("{} exited with status {status}", spec.program),
                false,
            ));
        }

        Ok(CommandOutput {
            status,
            stdout: output.stdout,
            stderr: output.stderr,
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
}
