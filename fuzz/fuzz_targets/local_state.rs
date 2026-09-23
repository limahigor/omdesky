#![no_main]

use libfuzzer_sys::fuzz_target;
use omdesky_platform::{config::Config, session_record::parse_start_ticks};

fuzz_target!(|data: &[u8]| {
    if let Ok(text) = std::str::from_utf8(data) {
        let _ = toml::from_str::<Config>(text);
        let _ = parse_start_ticks(text);
    }

    let _ = serde_json::from_slice::<omdesky_platform::session_record::SessionRecord>(data);
});
