const REPLACEMENT: char = '\u{fffd}';

pub const DISPLAY_NAME_LIMIT: usize = 96;
pub const DISPLAY_LINE_LIMIT: usize = 256;

pub fn sanitize_for_display(value: &str, limit: usize) -> String {
    let mut sanitized = String::with_capacity(value.len().min(limit));
    let mut truncated = false;

    for character in value.chars() {
        if sanitized.chars().count() >= limit {
            truncated = true;
            break;
        }

        sanitized.push(if is_presentation_safe(character) {
            character
        } else {
            REPLACEMENT
        });
    }

    if truncated {
        sanitized.push('…');
    }

    sanitized
}

pub fn sanitize_optional(value: Option<&str>, limit: usize) -> Option<String> {
    value.map(|value| sanitize_for_display(value, limit))
}

fn is_presentation_safe(character: char) -> bool {
    if character.is_control() {
        return false;
    }

    !is_bidi_or_invisible(character)
}

fn is_bidi_or_invisible(character: char) -> bool {
    matches!(
        character,
        '\u{00ad}'
            | '\u{061c}'
            | '\u{180e}'
            | '\u{200b}'..='\u{200f}'
            | '\u{202a}'..='\u{202e}'
            | '\u{2060}'..='\u{2064}'
            | '\u{2066}'..='\u{206f}'
            | '\u{feff}'
            | '\u{fff9}'..='\u{fffb}'
            | '\u{e0000}'..='\u{e007f}'
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sanitize_replaces_escape_and_osc_sequences() {
        let hostile = "desk\u{1b}]8;;https://evil.example\u{7}top";

        let sanitized = sanitize_for_display(hostile, DISPLAY_NAME_LIMIT);

        assert!(!sanitized.contains('\u{1b}'));
        assert!(!sanitized.contains('\u{7}'));
        assert!(sanitized.starts_with("desk\u{fffd}]8;;"));
    }

    #[test]
    fn test_sanitize_replaces_bidi_overrides() {
        let hostile = "safe\u{202e}gnp.exe";

        let sanitized = sanitize_for_display(hostile, DISPLAY_NAME_LIMIT);

        assert_eq!(sanitized, "safe\u{fffd}gnp.exe");
    }

    #[test]
    fn test_sanitize_replaces_c1_controls_and_newlines() {
        let sanitized = sanitize_for_display("a\nb\u{85}c\td", DISPLAY_NAME_LIMIT);

        assert_eq!(sanitized, "a\u{fffd}b\u{fffd}c\u{fffd}d");
    }

    #[test]
    fn test_sanitize_truncates_beyond_the_limit() {
        let sanitized = sanitize_for_display(&"x".repeat(200), 8);

        assert_eq!(sanitized, "xxxxxxxx…");
    }

    #[test]
    fn test_sanitize_preserves_ordinary_text() {
        assert_eq!(
            sanitize_for_display("desktop-a · 100.64.0.7", DISPLAY_NAME_LIMIT),
            "desktop-a · 100.64.0.7"
        );
    }

    #[test]
    fn test_sanitize_optional_maps_absent_values() {
        assert_eq!(sanitize_optional(None, DISPLAY_NAME_LIMIT), None);
        assert_eq!(
            sanitize_optional(Some("ok"), DISPLAY_NAME_LIMIT),
            Some("ok".to_owned())
        );
    }
}
