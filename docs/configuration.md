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

## Access control

Tailscale is the first access boundary. Your Tailscale policy must allow the controller to reach the remote computer's agent port.

Omdesky also has a local allowlist. An empty list accepts any caller that Tailscale can identify and route to the agent. Once you add an entry, only listed controllers are accepted.

Run these commands on the remote computer:

```bash
omdesky access allow controller-hostname
omdesky access list
omdesky access revoke controller-hostname
```

The allowlist is stored at `$XDG_STATE_HOME/omdesky/access/allowlist.json`, or `~/.local/state/omdesky/access/allowlist.json` when `XDG_STATE_HOME` is unset.

Use Tailscale hostnames or addresses when adding a controller. Omdesky resolves them to the stable Tailscale device identity before saving the entry.

## Sunshine credentials

`omdesky setup` stores the Sunshine web-interface username and password in the desktop user's Linux Secret Service collection. The credentials are not written to `config.toml` or another plaintext configuration file.

Run setup from an unlocked graphical session so the session D-Bus and Secret Service are available. The agent reads the keyring entry when approving a pairing request through Sunshine on the same computer, so restarting it is not required after updating the credentials.

Existing `sunshine-credentials.json` files are migrated into Secret Service and deleted after a successful migration.

## Settings reserved for later use

The current release stores but does not apply `general.notifications`, `general.default_input`, `input.escape_chord`, and `network.strict_tailnet_only`. Keep their default values unless you are testing upcoming behavior.
