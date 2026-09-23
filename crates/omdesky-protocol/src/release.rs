pub const RELEASE: &str = env!("CARGO_PKG_VERSION");
pub const RELEASE_HEADER: &str = "omdesky-release";

const MAX_RELEASE_BYTES: usize = 64;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ReleaseLine {
    pub major: u64,
    pub minor: u64,
}

impl ReleaseLine {
    pub fn parse(version: &str) -> Option<Self> {
        if version.is_empty() || version.len() > MAX_RELEASE_BYTES {
            return None;
        }

        let core = version
            .split_once(['-', '+'])
            .map_or(version, |(core, _)| core);

        let mut components = core.split('.');

        let major = parse_component(components.next()?)?;
        let minor = parse_component(components.next()?)?;
        parse_component(components.next()?)?;

        if components.next().is_some() {
            return None;
        }

        Some(Self { major, minor })
    }

    pub fn current() -> Option<Self> {
        Self::parse(RELEASE)
    }
}

pub fn is_compatible_release(version: &str) -> bool {
    let Some(current) = ReleaseLine::current() else {
        return false;
    };

    ReleaseLine::parse(version) == Some(current)
}

fn parse_component(component: &str) -> Option<u64> {
    let is_canonical = !component.is_empty()
        && component.bytes().all(|byte| byte.is_ascii_digit())
        && (component == "0" || !component.starts_with('0'));

    if !is_canonical {
        return None;
    }

    component.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_release_line_parses_major_and_minor() {
        assert_eq!(
            ReleaseLine::parse("0.2.7"),
            Some(ReleaseLine { major: 0, minor: 2 })
        );
    }

    #[test]
    fn test_release_line_ignores_prerelease_and_build_metadata() {
        assert_eq!(
            ReleaseLine::parse("1.4.0-rc.1+abc"),
            Some(ReleaseLine { major: 1, minor: 4 })
        );
    }

    #[test]
    fn test_release_line_rejects_malformed_versions() {
        for version in [
            "", "0.2", "0.2.x", "0.2.0.1", "v0.2.0", "00.2.0", " 0.2.0", "0..0",
        ] {
            assert_eq!(ReleaseLine::parse(version), None, "{version}");
        }
    }

    #[test]
    fn test_release_line_rejects_oversized_versions() {
        let version = format!("0.2.{}", "1".repeat(MAX_RELEASE_BYTES));

        assert_eq!(ReleaseLine::parse(&version), None);
    }

    #[test]
    fn test_current_release_is_parseable() {
        assert!(ReleaseLine::current().is_some());
        assert!(is_compatible_release(RELEASE));
    }

    #[test]
    fn test_compatible_release_accepts_other_patch_versions() {
        let current = ReleaseLine::current().expect("current release parses");

        let other_patch = format!("{}.{}.999", current.major, current.minor);

        assert!(is_compatible_release(&other_patch));
    }

    #[test]
    fn test_compatible_release_rejects_other_minor_and_major_versions() {
        let current = ReleaseLine::current().expect("current release parses");

        let previous_minor = format!("{}.{}.0", current.major, current.minor.wrapping_sub(1));
        let next_minor = format!("{}.{}.0", current.major, current.minor + 1);
        let next_major = format!("{}.{}.0", current.major + 1, current.minor);

        assert!(!is_compatible_release(&previous_minor));
        assert!(!is_compatible_release(&next_minor));
        assert!(!is_compatible_release(&next_major));
        assert!(!is_compatible_release("garbage"));
    }
}
