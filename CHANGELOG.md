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

- The session record stores the real Moonlight process and its start time, so `disconnect` no longer signals the controller itself or a process that reused the identifier.
- Selecting an already-connected device focuses the stream instead of toggling input capture.
- Sessions target the specific Moonlight window they started, so a second stream is no longer focused, closed or switched by mistake.
- Moonlight pairing drains both pipes and reaps the child on every path, so a failed pairing no longer leaves a process behind.
- Configuration and desktop launchers are written atomically and no longer follow a symbolic link.
- Bitrate conversion is checked, and resolution, frame rate and bitrate are range-validated.

### Changed

- **Breaking:** both computers must run this version. A controller from 0.1.1 sends no request identifier, so a newer agent refuses every command it issues; a newer controller cannot obtain a session lease or a pairing challenge from a 0.1.1 agent. Upgrade both ends together.
- **Breaking:** an installation that never populated its allowlist now refuses control requests until `omdesky access allow` names the other computer. Allowlist entries written by earlier versions keep working and are read as holding every capability.
- **Breaking:** the allowlist is consulted in both directions. An installation that listed the controller only on the computer being controlled will stream, but remote shortcuts and follow-focus stop working until the controller also lists that computer.

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
