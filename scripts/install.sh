#!/usr/bin/env bash
set -euo pipefail

REPO="limahigor/omdesky"
BIN_DIR="${OMDESKY_BIN_DIR:-$HOME/.local/bin}"
UNIT_DIR="$HOME/.config/systemd/user"

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
err() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

command -v tar >/dev/null 2>&1 || err "tar is required"

download() {
  local url="$1" out="$2"

  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$url" -o "$out"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO "$out" "$url"
  else
    err "curl or wget is required"
  fi
}

install_from_dir() {
  local src="$1"

  info "Installing binaries to $BIN_DIR"
  mkdir -p "$BIN_DIR"
  install -Dm755 "$src/omdesky" "$BIN_DIR/omdesky"
  install -Dm755 "$src/omdesky-agent" "$BIN_DIR/omdesky-agent"

  info "Installing user service to $UNIT_DIR"
  mkdir -p "$UNIT_DIR"
  install -Dm644 "$src/omdesky-agent.service" "$UNIT_DIR/omdesky-agent.service"

  systemctl --user daemon-reload >/dev/null 2>&1 || true
}

if [ -f "./omdesky" ] && [ -f "./omdesky-agent" ] && [ -f "./omdesky-agent.service" ]; then
  install_from_dir "."
else
  info "Resolving latest release for $REPO"
  tag="${OMDESKY_VERSION:-}"

  if [ -z "$tag" ]; then
    api="https://api.github.com/repos/$REPO/releases/latest"
    tmp_json="$(mktemp)"
    download "$api" "$tmp_json"
    tag="$(grep -m1 '"tag_name"' "$tmp_json" | sed -E 's/.*"tag_name" *: *"([^"]+)".*/\1/')"
    rm -f "$tmp_json"
  fi

  [ -n "$tag" ] || err "could not determine the latest release tag"

  version="${tag#v}"
  asset="omdesky-${version}-x86_64-linux"
  url="https://github.com/$REPO/releases/download/$tag/${asset}.tar.gz"

  info "Downloading $asset"
  workdir="$(mktemp -d)"
  trap 'rm -rf "$workdir"' EXIT
  download "$url" "$workdir/omdesky.tar.gz"
  tar -xzf "$workdir/omdesky.tar.gz" -C "$workdir"

  install_from_dir "$workdir/$asset"
fi

if ! printf '%s' ":$PATH:" | grep -q ":$BIN_DIR:"; then
  warn "$BIN_DIR is not in your PATH; add it so you can run omdesky"
fi

info "Installed. Next steps on the computer you want to control:"
printf '  omdesky setup\n'
printf '  systemctl --user enable --now omdesky-agent.service\n'
printf '  omdesky doctor\n'
