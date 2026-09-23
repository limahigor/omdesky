#![no_main]

use libfuzzer_sys::fuzz_target;
use omdesky_core::text::{DISPLAY_LINE_LIMIT, sanitize_for_display};

fuzz_target!(|data: &[u8]| {
    let Ok(text) = std::str::from_utf8(data) else {
        return;
    };

    let sanitized = sanitize_for_display(text, DISPLAY_LINE_LIMIT);

    assert!(
        !sanitized
            .chars()
            .any(|character| character.is_control() || character == '\u{202e}')
    );
});
