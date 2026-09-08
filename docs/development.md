# Development

## Prerequisites

DeskLink is a Rust 2024 workspace with a minimum supported Rust version of 1.88. Install a Rust toolchain with Cargo, rustfmt, and Clippy.

Most unit tests do not require a running Omarchy desktop. Testing runtime behavior requires Omarchy 4, Hyprland, Tailscale, Sunshine, Moonlight Qt, and a user systemd session.

## Build and run

Build all workspace crates:

```bash
cargo build --workspace
```

Build the release binaries with the locked dependency versions:

```bash
cargo build --release --locked --workspace
```

Run the CLI or terminal UI from the workspace:

```bash
cargo run -p omdesk-cli --bin omdesk -- --help
cargo run -p omdesk-cli --bin omdesk
```

Run the agent in the current desktop session:

```bash
cargo run -p omdesk-agent --bin omdesk-agent
```

The agent binds to the local Tailscale address. It exits if Omarchy 4 cannot be detected, Tailscale does not report a local address, or the listener cannot be created.

## Checks

The continuous integration workflow runs:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

Run the same commands before submitting a change.

The test suite covers domain validation, protocol serialization, command parsing and construction, Tailscale and Hyprland JSON mapping, Sunshine readiness and credentials, state files, launchers, theme loading, and terminal UI behavior. It does not exercise live Tailscale, Hyprland, Sunshine, Moonlight, or systemd services.

## Project structure

- `crates/omdesk-core`: domain types and validation
- `crates/omdesk-protocol`: HTTP protocol data types
- `crates/omdesk-application`: workflows and external-service interfaces
- `crates/omdesk-platform`: operating-system and application adapters
- `crates/omdesk-agent`: agent HTTP server
- `crates/omdesk-cli`: command-line executable
- `crates/omdesk-tui`: terminal interface
- `packaging/arch`: Arch package definition
- `packaging/systemd`: user service unit

See [Architecture](architecture.md) for the runtime relationships.

## Packaging

The package definition in `packaging/arch/PKGBUILD` builds the whole workspace and installs `omdesk`, `omdesk-agent`, the user service, and the license. It declares x86-64 Arch Linux and runtime dependencies on Tailscale, Sunshine, and Moonlight Qt.

The `PKGBUILD` has no source entries and expects Cargo sources in its working directory. Stage it with the repository contents in a package build directory before invoking `makepkg`; running it directly from `packaging/arch` will not find the workspace manifest.

## Runtime diagnostics

Use text output while developing interactively:

```bash
omdesk doctor
omdesk devices --all-tailnet
```

Use JSON when inspecting or scripting behavior:

```bash
omdesk doctor --json
omdesk devices --json
omdesk info HOST --json
omdesk displays HOST --json
omdesk workspaces HOST --json
omdesk windows HOST --json
```

Both executables initialize tracing from the standard `tracing_subscriber` environment filter. Set `RUST_LOG` when more runtime detail is needed, for example:

```bash
RUST_LOG=debug cargo run -p omdesk-agent --bin omdesk-agent
```
