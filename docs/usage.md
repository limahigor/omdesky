# User guide

This guide uses two names throughout:

- The **remote computer** is the Omarchy desktop you want to use.
- The **controller** is the computer in front of you.

A computer can serve both roles. Install `omdesk` and `omdesk-agent` on every computer where you want that flexibility.

## Before you begin

Connect both computers to the same Tailnet. The remote computer must be running Omarchy 4, Hyprland, Sunshine, and `omdesk-agent`. The controller must have Moonlight Qt installed.

## Prepare the remote computer

Open a terminal on the remote computer and run:

```bash
omdesk setup
```

Enter the username and password you use for Sunshine's web interface. Omarchy Desk stores them in the desktop user's Linux Secret Service collection, not in a plaintext configuration file. The credentials never leave the remote computer. Run setup from an unlocked graphical session so Secret Service is available.

Enable the agent:

```bash
systemctl --user enable --now omdesk-agent.service
```

Check that everything is ready:

```bash
omdesk doctor
```

The report checks Omarchy, Tailscale, Hyprland, Sunshine, and the credentials needed for automatic pairing.

If you built the project from source and have not installed the service, start the agent from the repository instead:

```bash
cargo run -p omdesk-agent --bin omdesk-agent
```

Keep that process running while you connect.

## Use the terminal interface

On the controller, run:

```bash
omdesk
```

Omarchy Desk scans your Tailnet and lists compatible devices. The main keys are:

- `Up`, `Down`, `j`, or `k` to select a device
- `Enter` to connect
- `r` to scan again
- `s` to change stream settings
- `q` or `Esc` to leave

The settings screen lets you choose the display behavior, resolution, frame rate, bitrate, codec, and audio preference. Changes are saved for future terminal-interface sessions.

## Connect from the command line

List available computers:

```bash
omdesk devices
```

Start a stream by using the displayed device name:

```bash
omdesk connect workstation
```

Pairing happens automatically when possible. To pair without opening a stream, run:

```bash
omdesk pair workstation
```

Useful connection examples:

```bash
omdesk connect workstation --windowed
omdesk connect workstation --width 2560 --height 1440 --fps 120
omdesk connect workstation --codec hevc --bitrate 30
omdesk connect workstation --workspace 2
omdesk connect workstation --input remote
```

Bitrate is measured in megabits per second. Available codecs are `auto`, `h264`, `hevc`, and `av1`.

## Control shortcuts during a stream

With remote input enabled, system shortcuts are sent to the remote desktop.

- Press `Super+R` to release shortcuts back to the controller.
- Press `Super+R` again to send shortcuts to the remote desktop.
- Press `Super+Q` to close the remote session.
- Use `Ctrl+Alt+Shift+Z` if you need Moonlight's built-in capture toggle.

After capture is released, shortcuts such as `Super+W` affect the local Moonlight window rather than the remote desktop.

## Inspect a remote desktop

The following commands are useful when choosing a monitor, workspace, or application:

```bash
omdesk info workstation
omdesk displays workstation
omdesk workspaces workstation
omdesk windows workstation
omdesk windows workstation --workspace 2
omdesk windows workstation --app-id firefox
```

Add `--json` when you need structured output for a script.

A target can be a visible Tailscale hostname, Tailnet IP address, stable Tailscale node ID, or a device name from your Omarchy Desk configuration.

## End a command-line session

A stream started with `omdesk connect` can be inspected or stopped from another terminal:

```bash
omdesk session
omdesk disconnect
```

The terminal interface manages its own session and does not create a command-line session record.

## Restrict access

Tailscale controls which devices can reach the agent. Omarchy Desk can add a second local restriction on the remote computer:

```bash
omdesk access allow controller-hostname
omdesk access list
omdesk access revoke controller-hostname
```

Once at least one controller is listed, other Tailscale devices are refused. Run these commands on each remote computer you want to protect.

## Troubleshooting

Start on the computer reporting the problem:

```bash
omdesk doctor
```

### No devices appear

- Confirm both computers are connected to Tailscale.
- Confirm the agent is running on the remote computer with `systemctl --user status omdesk-agent.service`.
- Run `omdesk devices --all-tailnet` to show computers whose agent cannot be reached.
- Check that your Tailscale policy allows the controller to reach the configured agent port.

### Sunshine is not ready

- Open Sunshine and confirm it is running.
- Confirm Hyprland reports an active monitor.
- Run `omdesk setup` again if the Sunshine password changed.
- Check that keyboard and mouse input are enabled in Sunshine.

### Pairing fails

- Run `omdesk setup` on the remote computer, not only on the controller.
- Confirm the stored username and password match Sunshine's web interface.
- Remove the old host from Moonlight and retry if the pairing state is stale.

If Moonlight shows a PIN and you need to approve it manually on the Sunshine computer, run:

```bash
omdesk sunshine-pin 1234 --name moonlight
```
