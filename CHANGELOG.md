# Changelog

All notable changes to Omdesky are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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

[0.1.1]: https://github.com/limahigor/omdesky/releases/tag/v0.1.1
[0.1.0]: https://github.com/limahigor/omdesky/releases/tag/v0.1.0
