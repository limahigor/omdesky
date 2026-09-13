# Omdesky

Omdesky lets you use one Omarchy computer from another through Tailscale. It finds your computers, prepares Sunshine on the remote machine, pairs it with Moonlight, and opens the stream from a terminal interface.

The video, sound, and input connection runs directly between Moonlight and Sunshine. Omdesky handles setup and desktop controls without carrying the stream itself.

## What you can do

- Find Omarchy computers on your Tailnet
- Connect from an interactive terminal interface
- Pair Moonlight and Sunshine
- Choose the resolution, frame rate, codec, bitrate, and window mode
- Follow the focused monitor while working on a multi-monitor desktop
- Inspect remote displays, workspaces, and windows from the command line
- Limit access to specific Tailscale devices

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

The fastest way is the one-line installer. It downloads the latest prebuilt
binaries and installs the user service into your home directory:

```bash
curl -fsSL https://raw.githubusercontent.com/limahigor/omdesky/master/scripts/install.sh | bash
```

Binaries go to `~/.local/bin`. Make sure that directory is in your `PATH`.

### Install with pacman (Arch / Omarchy)

Prefer a package you can update and remove with `pacman`? Build the prebuilt
package from the repository:

```bash
git clone https://github.com/limahigor/omdesky.git
cd omdesky/packaging/arch
makepkg -p PKGBUILD-bin -si
```

To compile from source instead, use the standard `PKGBUILD` in the same
directory with `makepkg -si`.

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
systemctl --user enable --now omdesky-agent.service
```

`omdesky setup` asks for the username and password used to open Sunshine's web interface. These credentials stay on that computer in the desktop user's Linux Secret Service collection and are used only to approve Moonlight pairing requests.

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
