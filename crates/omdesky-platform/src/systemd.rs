use omdesky_application::ports::{CommandRunner, CommandSpec, PortResult};
use std::path::PathBuf;

pub const AGENT_UNIT: &str = "omdesky-agent.service";

pub async fn agent_service_executable(runner: &dyn CommandRunner) -> PortResult<Option<PathBuf>> {
    let output = runner
        .run(CommandSpec::new(
            "systemctl",
            [
                "--user".to_owned(),
                "show".to_owned(),
                AGENT_UNIT.to_owned(),
                "--property=ExecStart".to_owned(),
                "--value".to_owned(),
            ],
        ))
        .await?;

    Ok(parse_exec_start(&String::from_utf8_lossy(&output.stdout)))
}

pub fn parse_exec_start(value: &str) -> Option<PathBuf> {
    value
        .trim()
        .trim_start_matches('{')
        .split(';')
        .map(str::trim)
        .find_map(|field| field.strip_prefix("path="))
        .map(str::trim)
        .filter(|path| path.starts_with('/'))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_exec_start_yields_the_service_executable() {
        let value = "{ path=/usr/bin/omdesky-agent ; argv[]=/usr/bin/omdesky-agent ; ignore_errors=no ; start_time=[n/a] ; stop_time=[n/a] ; pid=0 ; code=(null) ; status=0/0 }\n";

        assert_eq!(
            parse_exec_start(value),
            Some(PathBuf::from("/usr/bin/omdesky-agent"))
        );
    }

    #[test]
    fn test_a_missing_unit_yields_no_executable() {
        assert_eq!(parse_exec_start(""), None);
        assert_eq!(parse_exec_start("\n"), None);
    }

    #[test]
    fn test_a_relative_path_is_not_trusted() {
        assert_eq!(parse_exec_start("{ path=omdesky-agent ; argv[]=x }"), None);
    }
}
