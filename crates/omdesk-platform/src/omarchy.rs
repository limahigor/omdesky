use crate::process::TokioCommandRunner;
use async_trait::async_trait;
use omdesk_application::ports::{
    CommandRunner, CommandSpec, Notification, NotificationService, PortError, PortResult,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OmarchyVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub package_release: Option<String>,
}

pub fn parse_package_version(output: &str) -> PortResult<OmarchyVersion> {
    let version = output.split_whitespace().nth(1).ok_or_else(|| {
        PortError::new("OMARCHY_VERSION_UNKNOWN", "invalid package output", false)
    })?;

    let (upstream, package_release) = version
        .rsplit_once('-')
        .map_or((version, None), |(upstream, release)| {
            (upstream, Some(release.to_owned()))
        });

    let mut parts = upstream.split('.');
    let parse = |value: Option<&str>| {
        value
            .unwrap_or("0")
            .parse::<u32>()
            .map_err(|_| PortError::new("OMARCHY_VERSION_UNKNOWN", "invalid version", false))
    };

    let parsed = OmarchyVersion {
        major: parse(parts.next())?,
        minor: parse(parts.next())?,
        patch: parse(parts.next())?,
        package_release,
    };

    if parsed.major != 4 {
        return Err(PortError::new(
            "OMARCHY_UNSUPPORTED_VERSION",
            format!(
                "Omarchy {} is unsupported, version 4 is required",
                parsed.major
            ),
            false,
        ));
    }
    Ok(parsed)
}

pub async fn detect_version(runner: &dyn CommandRunner) -> PortResult<OmarchyVersion> {
    let output = runner
        .run(CommandSpec::new(
            "pacman",
            ["-Q".to_owned(), "omarchy".to_owned()],
        ))
        .await?;
    parse_package_version(&String::from_utf8_lossy(&output.stdout))
}

#[derive(Clone, Debug, Default)]
pub struct OmarchyNotificationAdapter {
    runner: TokioCommandRunner,
}

#[async_trait]
impl NotificationService for OmarchyNotificationAdapter {
    async fn send(&self, notification: Notification) -> PortResult<()> {
        self.runner
            .run(CommandSpec::new(
                "omarchy-notification-send",
                [notification.summary, notification.body],
            ))
            .await
            .map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_package_version_separates_release() {
        let version = parse_package_version("omarchy 4.0.1-2\n").expect("supported version");

        assert_eq!(version.major, 4);
        assert_eq!(version.patch, 1);
        assert_eq!(version.package_release.as_deref(), Some("2"));
    }

    #[test]
    fn test_parse_package_version_rejects_non_quattro() {
        let error = parse_package_version("omarchy 3.2.0-1").expect_err("unsupported");

        assert_eq!(error.code, "OMARCHY_UNSUPPORTED_VERSION");
    }
}
