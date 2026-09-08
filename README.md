# Omarchy DeskLink

Omarchy DeskLink connects one Omarchy desktop to another over a Tailnet. It discovers hosts through Tailscale, reads and focuses Hyprland workspaces and windows, coordinates Moonlight pairing with Sunshine, and launches the Moonlight stream. Video, audio, and input travel directly between Moonlight and Sunshine; the DeskLink agent handles only discovery and control requests.

Current features include:

- Tailnet discovery from the CLI or terminal UI
- Remote display, workspace, and window inspection
- Sunshine and Moonlight pairing
- Full-screen or windowed streaming with resolution, frame rate, codec, bitrate, and keyboard-capture options
- Optional access restriction by Tailscale stable node ID
- JSON output for discovery and inspection commands

## Requirements

DeskLink currently targets Omarchy 4 on x86-64 Arch Linux. Each host needs Tailscale and Hyprland. A controlled host also needs Sunshine; a controller needs the `moonlight` command from Moonlight Qt.

Building requires Rust 1.88 or newer.

## Build

Build the workspace:

```bash
cargo build --release --locked --workspace
```

The resulting executables are `target/release/omdesk` and `target/release/omdesk-agent`. The repository includes an Arch package definition and a user systemd unit under `packaging/`, but it does not include a complete source-install script.

## Quick start

On each controlled host, store the Sunshine admin credentials used for automated pairing, then start the agent:

```bash
omdesk setup
systemctl --user enable --now omdesk-agent.service
```

Both machines must be connected to Tailscale. On the controller:

```bash
omdesk devices
omdesk pair workstation
omdesk connect workstation
```

Run `omdesk` without a subcommand to open the terminal UI. Run `omdesk --help` or `omdesk <command> --help` for the complete command syntax.

## Documentation

- [User guide](docs/usage.md)
- [Configuration and access control](docs/configuration.md)
- [Architecture](docs/architecture.md)
- [Development](docs/development.md)

## License

DeskLink is licensed under the [MIT License](LICENSE).
