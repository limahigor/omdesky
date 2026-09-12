# How Omarchy Desk works

Omarchy Desk has a controller and an agent. The controller is the computer where you launch Moonlight. The agent runs on the Omarchy computer you want to use.

The agent helps the controller find the desktop, check Sunshine, approve pairing, and perform a small set of desktop actions. Once the stream starts, Moonlight connects directly to Sunshine. Closing the agent does not place an extra relay in the video or audio path.

```text
Setup and controls:
omdesk -> Tailscale -> omdesk-agent -> Hyprland and Sunshine

Video, audio, and input:
Moonlight -> Tailscale -> Sunshine
```

## Programs

### `omdesk`

`omdesk` provides both the command-line tools and the interactive terminal interface. It finds compatible computers through Tailscale, asks the agent about the remote desktop, pairs Moonlight with Sunshine, and starts Moonlight locally.

### `omdesk-agent`

`omdesk-agent` runs as the signed-in desktop user on the remote computer. It uses the current Hyprland session and binds to that computer's Tailscale address.

During startup, it checks the Omarchy version, obtains the local Tailscale identity, loads its saved device identity, and opens the configured agent port.

## What happens when you connect

When you run `omdesk connect workstation`, Omarchy Desk:

1. finds `workstation` in Tailscale;
2. asks its agent whether Sunshine is ready;
3. checks whether Moonlight already trusts that Sunshine host;
4. completes pairing when needed;
5. optionally focuses a requested workspace or window;
6. starts Moonlight against the remote Tailscale address;
7. restores local shortcut behavior when the stream ends.

For multi-monitor desktops, follow-focus watches which remote monitor has the focused window. It sends Moonlight's display-switch shortcut when the focus moves to another monitor.

## Pairing

Moonlight and Sunshine perform the actual pairing. Omarchy Desk only carries the one-time PIN between them:

1. Moonlight creates a four-digit PIN on the controller.
2. The controller sends that PIN to the remote agent.
3. The agent submits it to Sunshine on the same computer.
4. Moonlight confirms that pairing succeeded.

The Sunshine username and password remain on the remote computer. They are never sent to the controller.

## Network access

The agent listens on the local Tailscale address rather than a public network interface. Tailscale provides the private route and identifies the calling device.

The health check is available to any device that can reach the port. Every other request must resolve to a Tailscale device identity. A local Omarchy Desk allowlist can further restrict which Tailscale devices may control the computer.

See [Configuration and access control](configuration.md) for setup instructions.

## Safety boundaries

The agent accepts only the actions Omarchy Desk needs, such as listing displays, focusing a workspace, and managing a stream session. It does not provide a general remote shell or accept arbitrary Hyprland commands.

External programs are launched with separate argument values rather than commands assembled for a shell. Sunshine credentials stay on the remote computer in the desktop user's Linux Secret Service collection and are loaded only when needed.

## Source layout

The Rust workspace separates shared data, connection behavior, operating-system integrations, and user interfaces into individual crates. This keeps the network messages stable and limits desktop-specific behavior to the parts that call Tailscale, Hyprland, Sunshine, and Moonlight.

For build commands and crate details, see [Development](development.md).
