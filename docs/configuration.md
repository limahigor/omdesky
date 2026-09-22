# Configuration and access control

Omdesky works without a configuration file. Create one only when you want to change stream defaults, use a different agent port, or assign shorter names to devices.

The file is read from `$XDG_CONFIG_HOME/omdesky/config.toml` when `XDG_CONFIG_HOME` is set. Otherwise, use:

```text
~/.config/omdesky/config.toml
```

## Example

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

[display]
mode = "follow-focus"

[input]
escape_chord = "CTRL+ALT+SHIFT+Z"

[devices.office]
alias = "workstation"
default_display = "DP-1"
default_workspace = "2"
```

Missing settings use the built-in defaults.

## Stream settings

The terminal interface reads the values under `[stream]`:

- `width` and `height` set the stream resolution.
- `fps` sets the requested frame rate.
- `codec` accepts `auto`, `h264`, `hevc`, or `av1`.
- `bitrate_mbps` sets the bitrate in megabits per second. Use `0` to leave Moonlight's bitrate unchanged.
- `audio` stores the audio preference.

The command-line `connect` command uses its command-line defaults. Pass explicit options when you want a different resolution, frame rate, codec, or window mode.

## Display behavior

`display.mode = "follow-focus"` keeps the stream on the monitor currently focused in the remote Hyprland session. Switching focus to a window on another monitor changes the streamed display without restarting Moonlight or Sunshine.

## Device names

Entries under `[devices]` provide convenient command targets. In this example, `office` resolves through the alias `workstation`:

```bash
omdesky connect office
```

`default_display` and `default_workspace` are stored for future use and do not currently change a connection.

## Agent port

The default agent port is `48155`. Set the same value on every computer that runs the agent and every controller that connects to it:

```toml
[network]
agent_port = 48155
```

The environment variable `OMDESKY_AGENT_PORT` overrides the file for one process:

```bash
OMDESKY_AGENT_PORT=49000 omdesky devices
```

The agent listens only on the local Tailscale address. `allow_unsafe_wildcard_bind` disables the address-range safety check, but it does not make the agent listen on every network interface.

`strict_tailnet_only` is enforced: with its default of `true`, a request from an address outside `100.64.0.0/10` and `fd7a:115c:a1e0::/48` is refused before the agent resolves an identity or runs any subprocess.

## Access control

Tailscale is the first access boundary. Your Tailscale policy must allow the controller to reach the remote computer's agent port.

Omdesky also has a local allowlist, and it is the deciding one. **A missing or empty allowlist accepts nobody.** Until you add an entry, every request except the health check is refused, including from devices your Tailscale policy allows.

Run these commands on the computer you want to control:

```bash
omdesky access allow controller-hostname
omdesky access list
omdesky access revoke controller-hostname
```

`omdesky access allow` writes the file directly and never goes through the network, so the first controller is always added locally.

Both computers need an entry. The controller sends commands to the remote agent, and the remote agent sends shortcut and display-switch commands back to the controller's agent, so each one must list the other.

The allowlist is stored at `$XDG_STATE_HOME/omdesky/access/allowlist.json`, or `~/.local/state/omdesky/access/allowlist.json` when `XDG_STATE_HOME` is unset. It is written with owner-only permissions.

Use Tailscale hostnames or addresses when adding a controller. Omdesky resolves them to the stable Tailscale device identity before saving the entry.

### Capabilities

An allowlist entry grants a set of capabilities. `omdesky access allow` grants all of them unless you narrow the grant:

| Capability | Allows |
|---|---|
| `read_metadata` | Reading the node, displays, workspaces, windows and Sunshine status |
| `focus_workspace` | Focusing a workspace or a window |
| `control_session` | Attaching, renewing and detaching the desktop session |
| `send_shortcut` | Injecting a shortcut and switching the streamed display |
| `close_stream` | Closing the streamed window |
| `approve_pairing` | Approving a Sunshine pairing PIN |

```bash
omdesky access allow laptop --capability read_metadata --capability focus_workspace
```

A device granted only `read_metadata` can inspect the desktop but cannot take input ownership or approve pairing. Changes take effect within a couple of seconds; the agent does not need restarting.

Entries written before capabilities existed keep every capability when the file is read.

## Sessions and leases

Only one controller owns a desktop session at a time. When a controller attaches, the agent issues a session identifier, a generation number and a lease. Every later change to that session must present them, so a second controller cannot replace or end a session it does not own, and a late message from a finished session cannot end a newer one.

The controller renews the lease while the stream runs. If the controller crashes, is killed, or loses the network, the lease expires and the agent removes the session keybindings and restores local input by itself. The agent also clears leftover keybindings when it starts and when it is stopped.

## Sunshine credentials

`omdesky setup` stores the Sunshine web-interface username and password in the desktop user's Linux Secret Service collection. The credentials are not written to `config.toml` or another plaintext configuration file.

Run setup from an unlocked graphical session so the session D-Bus and Secret Service are available. The agent reads the keyring entry when approving a pairing request through Sunshine on the same computer, so restarting it is not required after updating the credentials.

Existing `sunshine-credentials.json` files are migrated into Secret Service and deleted after a successful migration.

## Settings reserved for later use

The current release stores but does not apply `general.notifications`, `general.default_input`, and `input.escape_chord`. Keep their default values unless you are testing upcoming behavior.

## Rejected and rate-limited requests

The agent bounds how much work an unauthorized peer can cause. Identity lookups are cached and capped, and a peer that sends too many requests receives `429` until it slows down. Sunshine pairing is additionally limited per identity and per time window.
