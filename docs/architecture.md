# How Omdesky works

Omdesky has a controller and an agent. The controller is the computer where you launch Moonlight. The agent runs on the Omarchy computer you want to use.

The agent helps the controller find the desktop, check Sunshine, approve pairing, and perform a small set of desktop actions. Once the stream starts, Moonlight connects directly to Sunshine. Closing the agent does not place an extra relay in the video or audio path.

```text
Setup and controls:
omdesky -> Tailscale -> omdesky-agent -> Hyprland and Sunshine

Video, audio, and input:
Moonlight -> Tailscale -> Sunshine
```

## Programs

### `omdesky`

`omdesky` provides both the command-line tools and the interactive terminal interface. It finds compatible computers through Tailscale, asks the agent about the remote desktop, pairs Moonlight with Sunshine, and starts Moonlight locally.

### `omdesky-agent`

`omdesky-agent` runs as the signed-in desktop user on the remote computer. It uses the current Hyprland session and binds to that computer's Tailscale address.

During startup, it checks the Omarchy version, obtains the local Tailscale identity, loads its saved device identity, and opens the configured agent port.

## What happens when you connect

When you run `omdesky connect workstation`, Omdesky:

1. finds `workstation` in Tailscale;
2. asks its agent whether Sunshine is ready;
3. checks whether Moonlight already trusts that Sunshine host;
4. completes pairing when needed;
5. optionally focuses a requested workspace or window;
6. starts Moonlight against the remote Tailscale address;
7. restores local shortcut behavior when the stream ends.

For multi-monitor desktops, follow-focus watches which remote monitor has the focused window. It sends Moonlight's display-switch shortcut when the focus moves to another monitor.

## Session ownership

A stream that owns input is a session, and a session has exactly one owner.

When the controller attaches, the agent issues a session identifier, a generation number and a lease. The owner is the Tailscale identity the request authenticated as, never a value taken from the message body. Every later change to that session presents the identifier and generation it was issued, so a message from a superseded session is ignored rather than applied, and another controller cannot take or end a live session.

Attaching and detaching are serialized and ordered. Attach installs the keybindings before publishing the session and rolls them back if the install fails; detach clears them before removing the session, and keeps the session when cleanup fails so it can be retried.

The controller renews the lease while the stream runs. If it stops renewing, for any reason including being killed, the lease expires and the agent removes the keybindings and restores local input. The agent clears leftover keybindings at startup and on shutdown as well.

The controller resolves the Hyprland address of its own Moonlight window from the child process id, so focusing, closing and switching a display act on one specific stream even when several are open.

## Pairing

Moonlight and Sunshine perform the actual pairing. Omdesky only carries the one-time PIN between them:

1. The controller asks the remote agent for a pairing challenge.
2. Moonlight creates a four-digit PIN on the controller.
3. The controller sends that PIN with the challenge to the remote agent.
4. The agent checks that the challenge is one it issued to that identity, is unused and has not expired, then submits the PIN to Sunshine on the same computer.
5. Moonlight confirms that pairing succeeded.

The challenge is single-use and pairing is rate limited per identity, so an authorized device cannot flood Sunshine with PIN attempts. Approving a pairing needs the `approve_pairing` capability, which is separate from reading the desktop or moving focus.

The Sunshine username and password remain on the remote computer. They are never sent to the controller.

## Network access

The agent listens on the local Tailscale address rather than a public network interface. Tailscale provides the private route and identifies the calling device.

The health check is available to any device that can reach the port. Every other request must resolve to a Tailscale device identity **and** appear in the local Omdesky allowlist with the capability that request needs. A missing or empty allowlist accepts nobody.

A session needs both directions. Before pairing or streaming, the controller confirms that its own allowlist grants the other computer `send_shortcut` and `close_stream`, the capabilities used to send shortcuts, display switches and stream closes back. When the controlled computer receives the session request it asks the controller's `/v1/capabilities` which grants it holds, and refuses the session if either is missing or the controller cannot be reached.

Both computers must run the same release line: the same major and minor version, with any patch version. Every request carries the sender's release in an `omdesky-release` header and every response carries the agent's, and either side refuses the other when the lines differ or the header is missing. The health check stays open to every release so a controller can report a device as incompatible instead of hiding it.

See [Configuration and access control](configuration.md) for setup instructions.

## Safety boundaries

The agent accepts only the actions Omdesky needs, such as listing displays, focusing a workspace, and managing a stream session. It does not provide a general remote shell or accept arbitrary Hyprland commands.

External programs are launched with separate argument values rather than commands assembled for a shell, are resolved from a fixed set of system directories rather than `PATH`, and run with a trusted `PATH` and a reduced environment. Sunshine credentials stay on the remote computer in the desktop user's Linux Secret Service collection and are loaded only when needed.

Text that another computer supplies — hostnames, window titles, display names, versions — is stripped of escape, control and bidirectional characters before it is displayed, so a hostile device cannot rewrite what you see in the terminal.

## Source layout

The Rust workspace separates shared data, connection behavior, operating-system integrations, and user interfaces into individual crates. This keeps the network messages stable and limits desktop-specific behavior to the parts that call Tailscale, Hyprland, Sunshine, and Moonlight.

For build commands and crate details, see [Development](development.md).
