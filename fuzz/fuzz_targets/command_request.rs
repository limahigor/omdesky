#![no_main]

use libfuzzer_sys::fuzz_target;
use omdesky_protocol::CommandRequest;

fuzz_target!(|data: &[u8]| {
    if let Ok(request) = serde_json::from_slice::<CommandRequest>(data) {
        let _ = request.command.validate();
        let _ = request.command.required_capability();
        let _ = serde_json::to_vec(&request);
    }
});
