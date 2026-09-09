# Architecture

Omarchy Desk separates control from streaming. The controller uses the agent to inspect and prepare a remote Omarchy session, then launches Moonlight directly against Sunshine. The agent is not in the stream path.

```text
Control:
omdesk -> HTTP over Tailscale -> omdesk-agent -> Tailscale / Hyprland / Sunshine API

Stream:
Moonlight -> Tailscale network -> Sunshine
```

## Programs

### `omdesk`

The controller program provides the command-line interface and terminal UI. It reads Tailscale peer state, probes agents, sends control requests, coordinates pairing, and starts the local `moonlight` process.

### `omdesk-agent`

The agent runs as the logged-in desktop user on the controlled host. At startup it:

1. verifies that Omarchy major version 4 is installed;
2. loads or creates a persistent Omarchy Desk node UUID;
3. obtains the local Tailscale address;
4. binds the control API to that address and the configured port.

The agent exposes versioned JSON endpoints for node metadata, displays, workspaces, windows, focus actions, Sunshine status, and pairing. It does not expose a general command endpoint or a raw Hyprland dispatch endpoint.

## Connection flow

For `omdesk connect TARGET`, the controller:

1. resolves the target from a Tailnet IP, Tailscale peer, or configured device entry;
2. requests the remote Sunshine status;
3. checks whether Moonlight is paired with that host;
4. runs the pairing flow when needed;
5. optionally asks the agent to focus a workspace or window;
6. launches `moonlight stream` directly against the host address;
7. waits for Moonlight to exit.

Workspace and window operations are narrow Hyprland actions. The agent executes `hyprctl` JSON queries and validated workspace or window focus commands. Window handles must use Hyprland's `0x` hexadecimal address form, and named workspaces cannot be empty or contain whitespace.

## Pairing flow

Omarchy Desk coordinates the existing Moonlight and Sunshine pairing process:

1. the controller starts `moonlight pair HOST`;
2. Omarchy Desk reads a four-digit PIN from Moonlight's standard output;
3. the controller sends the PIN and client name to the remote agent;
4. the agent submits them to Sunshine's local HTTPS API at `127.0.0.1:47990` using credentials stored on that host;
5. the controller asks Moonlight for the host's pairing state again.

Sunshine credentials stay on the controlled host and are not included in the Omarchy Desk API. Moonlight and Sunshine retain ownership of the pairing protocol and stream connection.

## Network and authorization

The agent control API uses HTTP on the host's Tailscale address. The health endpoint is open to callers that can reach the listener. Other endpoints derive the caller identity by running `tailscale whois --json` against the connection's source IP.

An empty local allowlist accepts any source with a Tailscale stable node ID, subject to Tailscale network policy. A nonempty allowlist adds an exact stable-node-ID check. See [Configuration and access control](configuration.md) for operational details.

Sunshine exposes a separate local HTTPS API. Omarchy Desk accepts Sunshine's self-signed certificate because requests are made only to the hard-coded loopback endpoint.

## Workspace layout

The Rust workspace is divided by role:

- `omdesk-core` contains domain values for nodes, displays, workspaces, windows, streams, input modes, and sessions.
- `omdesk-protocol` defines protocol version 1 request, response, and error types.
- `omdesk-application` contains discovery, pairing, and connection workflows plus the interfaces they use.
- `omdesk-platform` implements those interfaces with Tailscale, Hyprland, Moonlight, Sunshine, files, processes, notifications, and desktop launchers.
- `omdesk-agent` contains the Axum HTTP API and agent executable.
- `omdesk-cli` contains the Clap command-line executable and starts the terminal UI when no subcommand is given.
- `omdesk-tui` contains the Ratatui interface.

This arrangement keeps external command formats and filesystem details out of the application workflows and protocol data types.
