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
omdesky connect workstation --input remote
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
omdesky info workstation
omdesky displays workstation
omdesky workspaces workstation
omdesky windows workstation
omdesky windows workstation --workspace 2
omdesky windows workstation --app-id firefox
```

Add `--json` when you need structured output for a script.

A target can be a visible Tailscale hostname, Tailnet IP address, stable Tailscale node ID, or a device name from your Omdesky configuration.

## End a command-line session

A stream started with `omdesky connect` can be inspected or stopped from another terminal:

```bash
omdesky session
omdesky disconnect
```

The terminal interface manages its own session and does not create a command-line session record.

## Restrict access

Tailscale controls which devices can reach the agent. Omdesky can add a second local restriction on the remote computer:

```bash
omdesky access allow controller-hostname
omdesky access list
omdesky access revoke controller-hostname
```

Once at least one controller is listed, other Tailscale devices are refused. Run these commands on each remote computer you want to protect.

## Troubleshooting

Start on the computer reporting the problem:

```bash
omdesky doctor
```

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
