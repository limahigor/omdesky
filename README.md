# Omdesky

Omdesky lets you use one Omarchy computer from another through Tailscale. It finds your computers, prepares Sunshine on the remote machine, pairs it with Moonlight, and opens the stream from a terminal interface.

The video, sound, and input connection runs directly between Moonlight and Sunshine. Omdesky handles setup and desktop controls without carrying the stream itself.

Everything Omdesky does is available from the terminal interface. Run `omdesky` with no arguments to set up credentials, scan devices, connect, pair, and change stream settings without memorizing any commands. The CLI subcommands shown throughout this guide exist for scripting and quick access, but each one has an equivalent inside the interface.

## What you can do

- Find Omarchy computers on your Tailnet
- Connect from an interactive terminal interface
- Pair Moonlight and Sunshine
- Choose the resolution, frame rate, codec, bitrate, and window mode
- Follow the focused monitor while working on a multi-monitor desktop
- Inspect remote displays, workspaces, and windows from the command line
- Limit access to specific Tailscale devices, per capability

## Requirements

Omdesky currently supports Omarchy 4 on x86-64 systems.

On every computer you want to control:

- Tailscale must be connected
- Sunshine must be installed and running
- Hyprland must be running in the current desktop session
- `omdesky-agent` must run as your desktop user

On the computer you use as the controller:

- Tailscale must be connected
- Moonlight Qt must be installed
- The `moonlight` command must be available

## Install

### Install with pacman (Arch / Omarchy)

This is the recommended method. The package pins the SHA-256 of the release
tarball, so `makepkg` refuses to install an artifact that does not match what
the repository committed:

```bash
git clone https://github.com/limahigor/omdesky.git
cd omdesky/packaging/arch
makepkg -p PKGBUILD-bin -si
```

### Install with the standalone script

Download the installer, read it, then run it. The installer verifies the
SHA-256 of the release tarball and rejects archives containing absolute paths,
`..` components, or links:

```bash
curl -fsSL -o install-omdesky.sh https://raw.githubusercontent.com/limahigor/omdesky/master/scripts/install.sh
less install-omdesky.sh
bash install-omdesky.sh
```

By default the installer downloads the checksum published next to the release
archive, which only protects against corruption and partial tampering. Pin a
checksum you obtained independently to also protect against a compromised
release:

```bash
OMDESKY_SHA256=<digest> bash install-omdesky.sh
```

The standalone installer places binaries in `~/.local/bin` and writes a user service that points to that absolute location. Set `OMDESKY_BIN_DIR` to use another user-owned directory. Pass `--local <directory>` to install files you already have instead of downloading a release; the installer never picks up artifacts from the current directory on its own.

Do not mix this method with the Arch package. Before switching to the package, remove `~/.local/bin/omdesky`, `~/.local/bin/omdesky-agent`, and `~/.config/systemd/user/omdesky-agent.service`, then run `systemctl --user daemon-reload`.

To compile from source instead, use the standard `PKGBUILD` in the same
`packaging/arch` directory with `makepkg -si`. Both packages install the binaries in `/usr/bin` and the user service in `/usr/lib/systemd/user`.

## Build from source

Install Rust 1.88 or newer, then run:

```bash
cargo build --release --locked --workspace
```

The binaries are created at:

```text
target/release/omdesky
target/release/omdesky-agent
```

Copy both files to a directory in your `PATH`. If you want the agent to start with your desktop session, also install `packaging/systemd/omdesky-agent.service` as a user service.

## Set up a computer for remote access

Run these commands on the computer you want to control:

```bash
omdesky setup
omdesky access allow controller-hostname
systemctl --user enable --now omdesky-agent.service
```

`omdesky setup` asks for the username and password used to open Sunshine's web interface. These credentials stay on that computer in the desktop user's Linux Secret Service collection and are used only to approve Moonlight pairing requests.

`omdesky access allow` is required, not optional. The agent refuses every control request until the controller is listed. Run it on the controller too, naming this computer, because the remote desktop sends shortcut and display-switch commands back.

Check the setup with:

```bash
omdesky doctor
```

## Connect

Make sure both computers are online in Tailscale. On the controller, open the terminal interface:

```bash
omdesky
```

Select a ready device with the arrow keys and press `Enter` to connect. Press `s` to change stream settings or `r` to scan again.

You can also connect directly from the command line:

```bash
omdesky devices
omdesky connect workstation
```

Omdesky pairs Moonlight and Sunshine automatically when needed.

## Shortcuts during a stream

Remote input mode lets Omdesky send system shortcuts to the remote desktop.

- `Super+R` switches shortcut capture between the remote and local desktops
- `Super+Q` closes the remote session
- `Ctrl+Alt+Shift+Z` remains available as Moonlight's fallback capture shortcut

When capture is released, local shortcuts work normally on the controller.

## Common commands

```bash
omdesky devices
omdesky pair workstation
omdesky connect workstation --windowed
omdesky connect workstation --width 2560 --height 1440 --fps 120
omdesky doctor
```

Run `omdesky --help` or `omdesky <command> --help` to see every available option.

## More help

- [User guide](docs/usage.md)
- [Configuration and access control](docs/configuration.md)
- [Troubleshooting and development](docs/development.md)
- [How Omdesky works](docs/architecture.md)

## License

Omdesky is available under the [MIT License](LICENSE).
