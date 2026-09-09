# Configuration and access control

Omarchy Desk loads optional TOML configuration from:

- `$XDG_CONFIG_HOME/omdesk/config.toml`, when `XDG_CONFIG_HOME` is set
- `~/.config/omdesk/config.toml` otherwise

Missing files, sections, and fields use built-in defaults.

## Configuration reference

```toml
[general]
notifications = true
default_input = "remote"

[network]
agent_port = 48155
strict_tailnet_only = true
allow_unsafe_wildcard_bind = false

[stream]
width = 1920
height = 1080
fps = 60
codec = "auto"
audio = true
bitrate_mbps = 0

[input]
escape_chord = "CTRL+ALT+SHIFT+Z"

[files]
enabled = false
inbox = "~/Downloads/OmarchyDesk"

[devices.office]
alias = "workstation"
default_display = "DP-1"
default_workspace = "2"
```

`general.default_input` accepts `local` or `remote` and defaults to `remote`. The terminal UI always captures system shortcuts so they reach the remote desktop. Press `Ctrl+Alt+Shift+Z` in Moonlight to unlock input; local shortcuts such as `Super+W` then affect the stream window. CLI `connect` defaults to `local` unless `--input` is supplied.

`network.agent_port` is used by the agent listener and by clients during discovery and control requests. `OMDESK_AGENT_PORT` overrides it and must be an integer from 0 through 65535.

`network.allow_unsafe_wildcard_bind` disables the agent's built-in check that its selected address resembles a Tailscale address. It does not change the listener to a wildcard address; the agent still binds the address returned by Tailscale.

The terminal UI uses the values under `stream`. CLI `connect` has fixed defaults for width, height, FPS, codec, and audio. It uses `stream.bitrate_mbps` only when `--bitrate` is absent. A bitrate of `0` leaves Moonlight's bitrate unchanged.

A key under `devices` can be used as a command target. Its `alias` is substituted before Omarchy Desk matches a Tailscale peer. `default_display` and `default_workspace` are currently not used.

The current implementation does not consult `general.notifications`, `general.default_input`, `input.escape_chord`, `network.strict_tailnet_only`, or the `files` section. Audio values are parsed but are not translated into Moonlight arguments.

## Network boundary

The agent selects the first IPv4 Tailscale address reported for the local node, or the first available Tailscale address when no IPv4 address is present. It listens on that address and `network.agent_port`.

Omarchy Desk control requests use plain HTTP over the Tailnet. Omarchy Desk does not add TLS or bearer tokens to this connection. Tailscale provides the network path and source identity, and Tailscale Grants determine which peers can reach the listener.

`GET /v1/health` is unauthenticated and returns only agent status, protocol version, and agent version. For every other endpoint, the agent passes the TCP source address to `tailscale whois --json` and rejects callers that do not resolve to a Tailscale stable node ID.

## Access allowlist

By default, an empty Omarchy Desk allowlist accepts any caller that Tailscale identifies and permits to reach the agent. To restrict agent requests further, add controller identities on the controlled host:

```bash
omdesk access allow controller-hostname
omdesk access list
omdesk access revoke controller-hostname
```

`allow` accepts a visible Tailscale hostname, stable node ID, or IP address and stores the resolved stable node ID. Once the list contains an entry, every authenticated agent endpoint requires an exact match. `revoke` is idempotent.

The list is stored at:

- `$XDG_STATE_HOME/omdesk/access/allowlist.json`, or
- `~/.local/state/omdesk/access/allowlist.json`

On Unix, the file is created with mode `0644`. Its JSON format is:

```json
[
  {
    "tailnet_node_id": "stable-tailscale-node-id",
    "label": "controller-hostname",
    "added_at": "2026-09-08T12:00:00Z"
  }
]
```

Access-list commands modify the local machine's file. Run them on each controlled host whose agent you want to restrict. Keep Tailscale Grants in place as the outer network policy.

## Other stored data

The agent creates a Omarchy Desk node UUID in:

- `$XDG_STATE_HOME/omdesk/identity/node.json`, or
- `~/.local/state/omdesk/identity/node.json`

This UUID identifies the Omarchy Desk installation in node metadata. It is not an authentication credential.

Sunshine credentials are stored in:

- `$XDG_CONFIG_HOME/omdesk/sunshine-credentials.json`, or
- `~/.config/omdesk/sunshine-credentials.json`

The file contains a JSON `username` and `password` and is created with mode `0600` on Unix. The agent reloads it for each Sunshine request, so running `omdesk setup` does not require an agent restart.
