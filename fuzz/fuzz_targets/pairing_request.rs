#![no_main]

use libfuzzer_sys::fuzz_target;
use omdesky_protocol::SunshinePairRequest;

fuzz_target!(|data: &[u8]| {
    if let Ok(request) = serde_json::from_slice::<SunshinePairRequest>(data) {
        let _ = request.validate();
        let _ = format!("{request:?}");
    }
});
