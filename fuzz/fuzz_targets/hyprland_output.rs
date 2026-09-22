#![no_main]

use libfuzzer_sys::fuzz_target;
use omdesky_platform::hyprland;

fuzz_target!(|data: &[u8]| {
    let _ = hyprland::parse_monitors(data);
    let _ = hyprland::parse_workspaces(data);
    let _ = hyprland::parse_windows(data);
    let _ = hyprland::parse_active_window(data);
    let _ = hyprland::window_address_for_process(data, 4242);
});
