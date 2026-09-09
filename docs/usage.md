# User guide

Omarchy Desk uses two programs:

- `omdesk-agent` runs as the desktop user on a machine that will be controlled.
- `omdesk` discovers agents, inspects remote desktops, pairs Moonlight with Sunshine, and starts streams.

A machine may run both programs. The agent listens on its Tailscale address and does not carry the Moonlight stream.

## Prepare a controlled host

Install and configure Tailscale and Sunshine, and make sure the graphical Hyprland session is running. Omarchy Desk requires Omarchy major version 4.

Run setup as the desktop user:

```bash
omdesk setup
```

Setup creates Omarchy Desk's state directories, prompts for the local Sunshine admin username and password if they have not already been stored, and runs environment checks. For noninteractive setup, provide both values:

```bash
OMDESK_SUNSHINE_USERNAME='username' \
OMDESK_SUNSHINE_PASSWORD='password' \
omdesk setup
```

The credentials are written to `$XDG_CONFIG_HOME/omdesk/sunshine-credentials.json`, or `~/.config/omdesk/sunshine-credentials.json` when `XDG_CONFIG_HOME` is unset. On Unix, Omarchy Desk creates this file with mode `0600`. The agent uses the credentials only to submit pairing PINs to Sunshine's HTTPS API on `127.0.0.1:47990`.

If installed from the Arch package, enable the user service:

```bash
systemctl --user enable --now omdesk-agent.service
```

When running from a source checkout instead:

```bash
cargo run -p omdesk-agent --bin omdesk-agent
```

The agent needs access to the graphical session, `hyprctl`, `tailscale`, `sunshine`, and the user's systemd instance.

## Check the local environment

```bash
omdesk doctor
```

This checks the detected Omarchy version, Tailscale identity, Hyprland monitor query, Sunshine readiness, and the local Sunshine credential file. Use `--json` for machine-readable output.

## Find hosts

```bash
omdesk devices
```

Omarchy Desk reads `tailscale status --json`, probes each peer on the configured agent port, and shows peers running protocol version 1. Add `--all-tailnet` to include peers whose agent is unreachable or incompatible:

```bash
omdesk devices --all-tailnet
omdesk devices --json
```

Commands that accept `TARGET` support a Tailnet IP, online Tailscale hostname, stable Tailscale node ID, DNS-name prefix, or a device name configured in `config.toml`.

## Inspect a host

```bash
omdesk info workstation
omdesk displays workstation
omdesk workspaces workstation
omdesk windows workstation
omdesk windows workstation --workspace 2
omdesk windows workstation --app-id firefox
```

`info`, `displays`, `workspaces`, and `windows` accept `--json`. Window filtering uses exact workspace IDs and exact `app_id` or class values.

## Pair and connect

Pair Moonlight on the controller with Sunshine on the controlled host:

```bash
omdesk pair workstation
```

Omarchy Desk starts `moonlight pair`, reads the four-digit PIN from Moonlight's standard output, and asks the remote agent to submit it to the local Sunshine API. The controlled host must have valid Sunshine credentials stored by `omdesk setup`.

Start a stream:

```bash
omdesk connect workstation
```

`connect` checks Sunshine readiness and pairs automatically when necessary. Its default stream is full-screen at 1920x1080 and 60 FPS, with automatic codec selection and local system-key handling.

Common options:

```bash
omdesk connect workstation --windowed
omdesk connect workstation --width 2560 --height 1440 --fps 120
omdesk connect workstation --codec hevc --bitrate 30
omdesk connect workstation --workspace 2
omdesk connect workstation --window 0x55c9ab12
omdesk connect workstation --input remote
```

Codec values are `auto`, `h264`, `hevc`, and `av1`. Bitrate is specified in megabits per second. `--input remote` tells Moonlight to capture system keys for the remote session. During the stream, press `Super+R` to toggle capture between the remote desktop and the local session: while captured, shortcuts such as `Super+W` act on the remote desktop; after unlocking, they act locally (so `Super+W` closes the Moonlight window). Moonlight's built-in `Ctrl+Alt+Shift+Z` still works as a fallback.

The `--workspace` value may be a numeric Hyprland workspace ID or a name without whitespace. `--window` requires the hexadecimal Hyprland window address shown by the agent API. Use `omdesk windows --json` to inspect window data.

`--no-audio` is accepted by the current CLI but does not change the generated Moonlight command.

## Terminal UI

Run:

```bash
omdesk
```

The terminal UI discovers nodes and supports connection, Sunshine credential setup, and access-list management. Streams capture system shortcuts by default, so combinations such as `Super+W` are sent to the remote desktop. Press `Super+R` at any time to toggle capture: once unlocked, `Super+W` closes the local Moonlight window; press `Super+R` again to hand shortcuts back to the remote desktop. Its main keys are:

- `j`, `k`, or the arrow keys to move
- `Enter` to connect
- `r` to refresh
- `s` to open settings
- `q` or `Esc` to quit

The terminal UI uses stream defaults from `config.toml`. CLI `connect` uses its own command-line defaults, except that the configured bitrate is used when `--bitrate` is omitted.

## Session commands

A CLI connection records temporary session metadata under `$XDG_RUNTIME_DIR/omdesk/current-session.json`.

```bash
omdesk session
omdesk session --json
omdesk input status
omdesk disconnect
```

`disconnect` signals the supervising `omdesk` process recorded in that file. Terminal UI connections do not create this record. An interrupted CLI process can leave stale metadata.

## Desktop launchers

The launcher commands create files in `~/.local/share/applications`:

```bash
omdesk launcher create NODE_UUID
omdesk launcher list
omdesk launcher remove NODE_UUID
```

The current launcher command requires a Omarchy Desk UUID and generates an `omdesk connect NODE_UUID` command. Target resolution normally matches Tailscale identities and hostnames rather than the Omarchy Desk UUID, so configure that UUID as a device key before relying on the generated launcher.

## Troubleshooting

Start with:

```bash
omdesk doctor
```

If no hosts appear:

- Confirm both machines are connected to Tailscale.
- Confirm `omdesk-agent.service` is running as the desktop user on the controlled host.
- Confirm the configured agent port is reachable under your Tailscale Grants.
- Use `omdesk devices --all-tailnet` to distinguish an unreachable agent from an undiscovered peer.

If the agent does not start:

- Check that `pacman -Q omarchy` reports Omarchy 4.
- Check that `tailscale status --json` returns a local Tailscale address.
- Check that the user service starts inside the graphical session.

If Sunshine is reported as not ready:

- Confirm `sunshine --version` succeeds.
- Confirm `systemctl --user is-active sunshine.service` succeeds.
- Confirm `hyprctl monitors -j` reports an enabled display.
- Check that `keyboard` and `mouse` are not set to `disabled` in `~/.config/sunshine/sunshine.conf`.

If pairing fails:

- Run `omdesk setup` on the controlled host, not only on the controller.
- Confirm the stored credentials match Sunshine's admin credentials.
- Confirm `moonlight pair` prints a four-digit PIN to standard output within 20 seconds.

For an SSH-assisted local pairing flow, run this on the Sunshine host after Moonlight displays a PIN:

```bash
omdesk sunshine-pin 1234 --name moonlight
```
