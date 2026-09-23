use crate::ports::{PortError, PortResult};
use omdesky_protocol::{HealthResponse, PROTOCOL, RELEASE, is_compatible_release};

pub fn ensure_compatible_agent(health: &HealthResponse) -> PortResult<()> {
    if health.protocol == PROTOCOL && is_compatible_release(&health.agent_version) {
        return Ok(());
    }

    Err(PortError::new(
        "VERSION_INCOMPATIBLE",
        format!(
            "the device runs Omdesky {} and this computer runs {RELEASE}",
            health.agent_version
        ),
        false,
    ))
}

pub fn ensure_controller_ready(
    local_agent: PortResult<HealthResponse>,
    sunshine_configured: bool,
) -> PortResult<()> {
    let health = local_agent.map_err(|_| {
        PortError::new(
            "LOCAL_AGENT_UNAVAILABLE",
            "the local omdesky-agent is not running",
            true,
        )
    })?;

    ensure_compatible_agent(&health)
        .map_err(|error| PortError::new("LOCAL_AGENT_INCOMPATIBLE", error.message, false))?;

    if !sunshine_configured {
        return Err(PortError::new(
            "LOCAL_SUNSHINE_UNCONFIGURED",
            "Sunshine credentials are not configured on this controller",
            false,
        ));
    }

    Ok(())
}

#[cfg(test)]
pub(crate) fn health(agent_version: &str) -> HealthResponse {
    HealthResponse {
        status: "ok".to_owned(),
        protocol: PROTOCOL,
        agent_version: agent_version.to_owned(),
    }
}

#[cfg(test)]
pub(crate) fn other_minor_release() -> String {
    let current = omdesky_protocol::ReleaseLine::current().expect("current release parses");

    format!("{}.{}.0", current.major, current.minor + 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_an_agent_from_the_same_release_line_is_compatible() {
        assert!(ensure_compatible_agent(&health(RELEASE)).is_ok());
    }

    #[test]
    fn test_an_agent_from_another_release_line_is_incompatible() {
        let error = ensure_compatible_agent(&health(&other_minor_release()))
            .expect_err("another release line is refused");

        assert_eq!(error.code, "VERSION_INCOMPATIBLE");
    }

    #[test]
    fn test_an_agent_speaking_another_protocol_is_incompatible() {
        let mut response = health(RELEASE);
        response.protocol = PROTOCOL + 1;

        assert!(ensure_compatible_agent(&response).is_err());
    }

    #[test]
    fn test_ensure_controller_ready_rejects_inactive_local_agent() {
        let local_agent = Err(PortError::new(
            "AGENT_UNREACHABLE",
            "connection refused",
            true,
        ));

        let error =
            ensure_controller_ready(local_agent, true).expect_err("inactive agent is rejected");

        assert_eq!(error.code, "LOCAL_AGENT_UNAVAILABLE");
    }

    #[test]
    fn test_ensure_controller_ready_rejects_local_agent_from_another_release() {
        let local_agent = Ok(health(&other_minor_release()));

        let error = ensure_controller_ready(local_agent, true)
            .expect_err("an agent from another release is rejected");

        assert_eq!(error.code, "LOCAL_AGENT_INCOMPATIBLE");
        assert!(!error.retryable);
    }

    #[test]
    fn test_ensure_controller_ready_rejects_missing_sunshine_configuration() {
        let error = ensure_controller_ready(Ok(health(RELEASE)), false)
            .expect_err("missing setup is rejected");

        assert_eq!(error.code, "LOCAL_SUNSHINE_UNCONFIGURED");
    }

    #[test]
    fn test_ensure_controller_ready_accepts_configured_controller() {
        assert!(ensure_controller_ready(Ok(health(RELEASE)), true).is_ok());
    }
}
