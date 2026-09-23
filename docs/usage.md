# User guide

This guide uses two names throughout:

- The **remote computer** is the Omarchy desktop you want to use.
- The **controller** is the computer in front of you.

A computer can serve both roles. Install `omdesky` and `omdesky-agent` on every computer where you want that flexibility.

## Before you begin

Connect both computers to the same Tailnet. The remote computer must be running Omarchy 4, Hyprland, Sunshine, and `omdesky-agent`. The controller must have Moonlight Qt installed.

## Prepare the remote computer

Open a terminal on the remote computer and run:

```bash
omdesky setup
```

Enter the username and password you use for Sunshine's web interface. Omdesky stores them in the desktop user's Linux Secret Service collection, not in a plaintext configuration file. The credentials never leave the remote computer. Run setup from an unlocked graphical session so Secret Service is available.

Enable the agent:

```bash
systemctl --user enable --now omdesky-agent.service
```

Check that everything is ready:

```bash
omdesky doctor
```

The report checks Omarchy, Tailscale, Hyprland, Sunshine, and the credentials needed for automatic pairing.

If you built the project from source and have not installed the service, start the agent from the repository instead:

```bash
cargo run -p omdesky-agent --bin omdesky-agent
```

Keep that process running while you connect.

## Use the terminal interface

On the controller, run:

```bash
omdesky
```

Omdesky scans your Tailnet and lists compatible devices. The main keys are:

- `Up`, `Down`, `j`, or `k` to select a device
- `Enter` to connect
- `r` to scan again
- `s` to change stream settings
- `q` or `Esc` to leave

The settings screen lets you choose the display behavior, resolution, frame rate, bitrate, codec, and audio preference. Changes are saved for future terminal-interface sessions.

## Connect from the command line

List available computers:

```bash
omdesky devices
```

Each device is `ready`, `blocked`, `offline` or `unavailable`. A blocked device is followed by one line per problem that says what is wrong and which command fixes it, and on which computer to run it.

Start a stream by using the displayed device name:

```bash
omdesky connect workstation
```

Pairing happens automatically when possible. To pair without opening a stream, run:

```bash
omdesky pair workstation
```

Useful connection examples:

```bash
omdesky connect workstation --windowed
omdesky connect workstation --width 2560 --height 1440 --fps 120
omdesky connect workstation --codec hevc --bitrate 30
omdesky connect workstation --workspace 2
omdesky connect workstation --input local
```

Bitrate is measured in megabits per second. Available codecs are `auto`, `h264`, `hevc`, and `av1`.

## Control shortcuts during a stream

Remote input is enabled by default, so system shortcuts are sent to the remote desktop. Pass `--input local` to keep shortcuts on the controller instead.

- Press `Super+R` to release shortcuts back to the controller.
- Press `Super+R` again to send shortcuts to the remote desktop.
- Press `Super+Q` to close the remote session.
- Use `Ctrl+Alt+Shift+Z` if you need Moonlight's built-in capture toggle.

After capture is released, shortcuts such as `Super+W` affect the local Moonlight window rather than the remote desktop.

## Inspect a remote desktop

The following commands are useful when choosing a monitor, workspace, or application:

```bash
omdesky info workstation
omdesky displays workstation
omdesky workspaces workstation
omdesky windows workstation
omdesky windows workstation --workspace 2
omdesky windows workstation --app-id firefox
```

Add `--json` when you need structured output for a script. Every JSON document is an object with a `schema` number, currently `1`, next to the requested data, such as `devices`, `displays`, `workspaces`, `windows`, `session` or `exit_status`. A script should check `schema` before reading the rest.

In `devices`, each entry carries `status` and a `blockers` list. Every blocker has a `code` (`incompatible`, `denied` or `needs_access`), the `side` that has to change (`local` or `remote`), and a `fix` with the command or action to take on that side; `fix` never names the computer, `side` does.

A target can be a visible Tailscale hostname, Tailnet IP address, stable Tailscale node ID, or a device name from your Omdesky configuration.

## End a command-line session

A stream started with `omdesky connect` can be inspected or stopped from another terminal:

```bash
omdesky session
omdesky disconnect
```

The terminal interface manages its own session and does not create a command-line session record.

## Allow a controller

Tailscale controls which devices can reach the agent. Omdesky decides which of them may actually control it, and it decides by an explicit list:

```bash
omdesky access allow controller-hostname
omdesky access list
omdesky access revoke controller-hostname
```

Until a controller is listed, every request except the health check is refused. Run these commands on each computer you want to control, and on each controller too: the remote computer sends shortcut and display-switch commands back to the controller, so both sides need an entry for the other. Omdesky checks both entries before it pairs or starts a stream and refuses the connection if either is missing; `omdesky devices` marks such a device as blocked and prints the `omdesky access allow` command to run, and on which computer.

Narrow a grant with `--capability` when a device should do less than everything:

```bash
omdesky access allow laptop --capability read_metadata --capability focus_workspace
```

The available capabilities are `read_metadata`, `focus_workspace`, `control_session`, `send_shortcut`, `close_stream` and `approve_pairing`. See [Configuration and access control](configuration.md) for what each one covers.

## Troubleshooting

Start on the computer reporting the problem:

```bash
omdesky doctor
```

Each check reports `PASS`, `WARN` or `FAIL`, and the command exits with a non-zero status when any check fails:

- `omarchy`, `tailscale`, `hyprland` and `sunshine` confirm the tools Omdesky drives are present and answering.
- `sunshine_pairing` confirms the Sunshine credentials are in the desktop keyring.
- `agent` confirms the local agent answers and runs the same release line and protocol as the command; after an upgrade it asks you to restart the service.
- `agent_service` compares the binary the user service runs with the one you invoked, which catches a packaged agent left running next to a newer standalone install.
- `access` warns when the allowlist is empty or holds entries from an earlier release that grant nothing.
- `devices` lists every blocked device with the command that fixes it and the computer to run it on.

### Read the agent log

The agent writes to the user journal at the `info` level:

```bash
omdesky-agent --version
journalctl --user -u omdesky-agent -e
```

For more detail, raise the level with a drop-in and restart the service:

```bash
systemctl --user edit omdesky-agent
# add:  [Service]
#       Environment=RUST_LOG=debug
systemctl --user restart omdesky-agent
```

The command-line tool and terminal interface stay silent unless `RUST_LOG` is set, so logging never draws over the interface or mixes with `--json` output; log lines always go to standard error.

### No devices appear

- Confirm both computers are connected to Tailscale.
- Confirm the agent is running on the remote computer with `systemctl --user status omdesky-agent.service`.
- Run `omdesky devices --all-tailnet` to show computers whose agent cannot be reached.
- Check that your Tailscale policy allows the controller to reach the configured agent port.

### Sunshine is not ready

- Open Sunshine and confirm it is running.
- Confirm Hyprland reports an active monitor.
- Run `omdesky setup` again if the Sunshine password changed.
- Check that keyboard and mouse input are enabled in Sunshine.

### Pairing fails

- Run `omdesky setup` on the remote computer, not only on the controller.
- Confirm the stored username and password match Sunshine's web interface.
- Remove the old host from Moonlight and retry if the pairing state is stale.

If Moonlight shows a PIN and you need to approve it manually on the Sunshine computer, run:

```bash
omdesky sunshine-pin 1234 --name moonlight
```
