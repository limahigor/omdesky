# Changelog

All notable changes to Omdesky are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Security

- Authorization is fail-closed. A missing or empty allowlist now refuses every request except the health check, instead of accepting any reachable Tailnet device.
- Allowlist entries carry capabilities, so a device can be granted reading, focus, session control, shortcut injection, stream closing or pairing approval separately.
- Desktop sessions have an owner, an unpredictable identifier, a generation and a lease. Only the owner can renew or end its session, a message from a superseded session is ignored, and an unrenewed lease expires and restores local input.
- Attach and detach are serialized and roll back, so a failing `hyprctl` no longer leaves a phantom session or orphaned keybindings. The agent also reconciles at startup and releases the session on shutdown.
- The controller callback endpoint must match the authenticated peer, which removes a blind request path into loopback and the LAN.
- `tailscale whois` results are cached and concurrency-capped, and a per-source rate limit sheds a flood before any subprocess is created.
- Sunshine pairing requires a single-use challenge the agent issued to that identity, with a TTL and per-identity rate limits.
- Commands carry a single-use request identifier and a timestamp; replayed and stale mutations are rejected.
- Provider tools are resolved from a fixed set of system directories rather than `PATH`, and run with a trusted `PATH` and a reduced environment.
- Text supplied by another computer is stripped of escape, control and bidirectional characters before display.
- `network.strict_tailnet_only` is enforced; the Tailscale address check uses the exact CGNAT and ULA ranges.
- The Hyprland event socket must be a socket owned by this user under `XDG_RUNTIME_DIR`; the `/tmp` fallback is removed.
- The installer verifies the release tarball's SHA-256 and rejects unsafe archive entries; the binary package pins the released checksum instead of `SKIP`.
- The user service sets a trusted `PATH` and a systemd sandbox profile.

### Fixed

- Release builds log. The agent writes structured events to the journal at `info` by default, the command-line tool logs to standard error when `RUST_LOG` is set, and both honor `RUST_LOG`.
- `omdesky-agent --version` and `--help` print instead of starting the agent, and unknown arguments are refused.
- `omdesky doctor` also checks that the local agent runs the same release and protocol, that the user service runs the binary from the same installation, that the allowlist grants something, and which devices are blocked and how to fix them. It exits with a non-zero status when a check fails.
- At startup the agent waits up to a minute for Tailscale instead of exiting, and startup failures report the error code and detail instead of a generic message.
- The agent reports its Tailscale hostname; under systemd it previously reported `omarchy` for every computer.
- The "operation took too long" message is shown for command timeouts again.

- The session record stores the real Moonlight process and its start time, so `disconnect` no longer signals the controller itself or a process that reused the identifier.
- Selecting an already-connected device focuses the stream instead of toggling input capture.
- Sessions target the specific Moonlight window they started, so a second stream is no longer focused, closed or switched by mistake.
- Moonlight pairing drains both pipes and reaps the child on every path, so a failed pairing no longer leaves a process behind.
- Configuration and desktop launchers are written atomically and no longer follow a symbolic link.
- Bitrate conversion is checked, and resolution, frame rate and bitrate are range-validated.

### Removed

- The `[general]`, `[input]` and `[files]` configuration sections and the `default_display` and `default_workspace` device settings, which were stored but never applied. Existing files that contain them still load.
- The migration of plaintext `sunshine-credentials.json` files, which no released version wrote.
- Unused domain types, port methods and protocol helpers left over from earlier designs.

### Changed

- The control protocol is version 2. Every command carries a required request identifier and timestamp, pairing requires its challenge identifier, and node information no longer lists protocol versions. A 0.1 controller now reports a 0.2 agent as incompatible instead of ready.
- Agent errors use one fixed vocabulary of codes, each with its own HTTP status and retry hint, and a malformed request body is answered with the same error envelope. The controller keeps every known code instead of collapsing most of them into a generic failure.
- Capability lists from another computer ignore values this version does not recognize.
- The wire contract and the `devices --json` document are recorded as fixtures bound to the release line, and the test suite fails when they change without a new minor version.

- **Breaking:** both computers must run the same release line, meaning the same major and minor version. Each side announces its release on every request and response and refuses a peer from another line, including 0.1.1, which announces none. A device from another line is listed as incompatible, and connecting to it fails with a clear message instead of a partial session. Upgrade both ends together.
- A local agent left running from an earlier release is reported as such, instead of as a stopped agent, so restarting it after an upgrade is the obvious fix.
- Devices whose agent is incompatible are listed without `--all-tailnet`, so an out-of-date computer is visible rather than missing.
- **Breaking:** an installation that never populated its allowlist now refuses control requests until `omdesky access allow` names the other computer. Allowlist entries written by earlier versions carry no capabilities and grant nothing; run `omdesky access allow` again to grant them.
- **Breaking:** the allowlist is consulted in both directions, and a connection now starts only when both are in place. The controller checks its own allowlist grants the other computer `send_shortcut` and `close_stream` before pairing or streaming, and the computer being controlled confirms those grants with the controller before it accepts the session. An installation that listed the controller only on the computer being controlled is refused with a message naming the missing entry, instead of streaming without shortcuts or follow-focus.
- A device is `ready`, `blocked`, `offline` or `unavailable`. A blocked device lists every problem found, each with the side that must change and the command that fixes it, so the terminal interface, the command line and the bar plugin give the same instructions.
- **Breaking for scripts:** every `--json` document is an object with a `schema` number next to the requested data; `omdesky devices --json` returns `{"schema": 1, "devices": [...]}` instead of a bare list.
- The `omdesky input` subcommand is removed. `input status` repeated `omdesky session`, and the other actions always failed.
- `/v1/capabilities` answers any listed device with its own grants, without requiring `read_metadata`.

## [0.1.1] - 2026-09-13

### Security

- Bound provider command and agent HTTP responses before deserialization.
- Limit Tailnet peer data and discovery concurrency to keep device listing predictable under hostile or unexpectedly large inputs.

### Fixed

- Generate standalone user services with the actual absolute `omdesky-agent` installation path.

## [0.1.0] - 2026-09-12

First public release. Omdesky lets you reach one Omarchy desktop from another
over Tailscale, handling discovery, pairing, and stream setup while Moonlight
and Sunshine carry the stream directly.

### Added

- Terminal interface to discover devices, connect, pair, and adjust stream settings.
- Command-line subcommands for scripting setup, device listing, pairing, and connecting.
- Follow-focus mode that tracks the active monitor across a multi-monitor desktop.
- Automatic Sunshine and Moonlight pairing when needed.
- Sessions that stop cleanly when a local or remote agent becomes unavailable.
- Sunshine credentials stored in the desktop user's Linux keyring.
- One-line installer and prebuilt Arch package for installation without the AUR.

[Unreleased]: https://github.com/limahigor/omdesky/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/limahigor/omdesky/releases/tag/v0.1.1
[0.1.0]: https://github.com/limahigor/omdesky/releases/tag/v0.1.0
