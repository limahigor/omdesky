# Development

This page covers building, testing, and diagnosing Omdesky from a source checkout.

## Requirements

Use Rust 1.88 or newer with Cargo, rustfmt, and Clippy. Most tests run on any Linux development machine. Testing real connections requires Omarchy 4, Hyprland, Tailscale, Sunshine, Moonlight Qt, and a user systemd session.

## Build and run

Build the workspace:

```bash
cargo build --workspace
```

Open the terminal interface:

```bash
cargo run -p omdesky-cli --bin omdesky
```

Run a command:

```bash
cargo run -p omdesky-cli --bin omdesky -- devices
```

Start the agent in the current desktop session:

```bash
cargo run -p omdesky-agent --bin omdesky-agent
```

The agent stops during startup if it cannot detect Omarchy 4, obtain a Tailscale address, or open its listener.

## Release build

Use the lockfile for distributable binaries:

```bash
cargo build --release --locked --workspace
```

Release builds do not initialize debug tracing and do not show debug-only desktop notifications.

## Checks

Run the same checks used by the project before submitting a change:

```bash
cargo fmt --all --check
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --release --workspace
```

Tests cover validation, protocol data, command construction, Tailscale and Hyprland response parsing, Sunshine readiness, credentials, state files, desktop launchers, themes, and terminal rendering. Credential tests use an in-memory backend and do not require a live D-Bus session or Secret Service. They do not replace testing with live desktop services.

## Debug output

Debug builds can emit structured runtime details through `RUST_LOG`:

```bash
RUST_LOG=debug cargo run -p omdesky-agent --bin omdesky-agent
RUST_LOG=debug cargo run -p omdesky-cli --bin omdesky -- devices
```

User-facing errors remain short and actionable. Debug output contains the underlying operation, error code, and technical detail needed for diagnosis. Do not include passwords or other secrets in logs.

For a quick environment check, run:

```bash
omdesky doctor
omdesky devices --all-tailnet
```

Add `--json` when comparing output in a script or test.

## Workspace layout

- `crates/omdesky-core` contains shared values and validation.
- `crates/omdesky-protocol` contains the JSON request and response types.
- `crates/omdesky-application` contains discovery, pairing, connection, and display behavior.
- `crates/omdesky-platform` integrates with Tailscale, Hyprland, Moonlight, Sunshine, files, processes, and desktop notifications.
- `crates/omdesky-agent` provides the service that runs on a remote computer.
- `crates/omdesky-cli` provides commands and starts the terminal interface.
- `crates/omdesky-tui` renders and controls the terminal interface.
- `packaging/arch` contains the Arch package definition.
- `packaging/systemd` contains the user service.

## Packaging

`packaging/arch/PKGBUILD` installs `omdesky`, `omdesky-agent`, the user service, and the license. It expects the repository contents to be available in the package build directory.

After installing the package, enable the agent for the current user:

```bash
systemctl --user enable --now omdesky-agent.service
```
