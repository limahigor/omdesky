#!/usr/bin/env bash
set -euo pipefail

REPO="${OMDESKY_REPO:-limahigor/omdesky}"
BIN_DIR="${OMDESKY_BIN_DIR:-$HOME/.local/bin}"
UNIT_DIR="$HOME/.config/systemd/user"
LOCAL_DIR=""

info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }
warn() { printf '\033[1;33mwarning:\033[0m %s\n' "$1" >&2; }
err() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

usage() {
  cat <<'USAGE'
Usage: install.sh [--local <directory>]

  --local <directory>  Install the omdesky, omdesky-agent and
                       omdesky-agent.service files already present in
                       <directory> instead of downloading a release.

Environment:
  OMDESKY_VERSION   Release tag to install (default: latest)
  OMDESKY_SHA256    Expected SHA-256 of the release tarball. When set, the
                    published checksum file is ignored and this value is
                    required to match.
  OMDESKY_BIN_DIR   Installation directory for the binaries
  OMDESKY_REPO      Source repository (default: limahigor/omdesky)
USAGE
}

while [ "$#" -gt 0 ]; do
  case "$1" in
    --local)
      [ "$#" -ge 2 ] || err "--local requires a directory"
      LOCAL_DIR="$2"
      shift 2
      ;;
    --local=*)
      LOCAL_DIR="${1#--local=}"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      err "unknown argument: $1"
      ;;
  esac
done

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

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | cut -d' ' -f1
  elif command -v shasum >/dev/null 2>&1; then
    shasum -a 256 "$1" | cut -d' ' -f1
  else
    err "sha256sum or shasum is required to verify the download"
  fi
}

verify_checksum() {
  local archive="$1" expected="$2" actual

  [ -n "$expected" ] || err "no expected checksum available; refusing to install"

  case "$expected" in
    [0-9a-f]*) [ "${#expected}" -eq 64 ] || err "malformed expected checksum" ;;
    *) err "malformed expected checksum" ;;
  esac

  actual="$(sha256_of "$archive")"

  if [ "$actual" != "$expected" ]; then
    err "checksum mismatch: expected $expected but the download is $actual"
  fi

  info "Verified SHA-256 $actual"
}

# Reject archives that would write outside the extraction directory or install
# links pointing at attacker-chosen paths.
verify_archive_layout() {
  local archive="$1" prefix="$2" entry

  tar -tzf "$archive" >/dev/null 2>&1 || err "the downloaded archive is not readable"

  while IFS= read -r entry; do
    case "$entry" in
      /*|*..*) err "archive entry '$entry' escapes the extraction directory" ;;
      "$prefix"/*|"$prefix") ;;
      *) err "archive entry '$entry' is outside '$prefix/'" ;;
    esac
  done < <(tar -tzf "$archive")

  if tar -tvzf "$archive" | grep -Eq '^[lh]'; then
    err "the archive contains symbolic or hard links"
  fi
}

install_from_dir() {
  local src="$1" file

  for file in omdesky omdesky-agent omdesky-agent.service; do
    [ -f "$src/$file" ] || err "missing $file in $src"
    [ -L "$src/$file" ] && err "$src/$file is a symbolic link"
  done

  info "Installing binaries to $BIN_DIR"
  mkdir -p "$BIN_DIR"
  BIN_DIR="$(cd "$BIN_DIR" && pwd)"

  case "$BIN_DIR" in
    *[!-A-Za-z0-9_/.]*) err "OMDESKY_BIN_DIR contains characters that cannot be written to a unit file" ;;
  esac

  install -Dm755 "$src/omdesky" "$BIN_DIR/omdesky"
  install -Dm755 "$src/omdesky-agent" "$BIN_DIR/omdesky-agent"

  info "Installing user service to $UNIT_DIR"
  mkdir -p "$UNIT_DIR"
  AGENT_EXEC_START="ExecStart=\"$BIN_DIR/omdesky-agent\"" awk '
    /^ExecStart=/ { print ENVIRON["AGENT_EXEC_START"]; next }
    { print }
  ' "$src/omdesky-agent.service" > "$UNIT_DIR/omdesky-agent.service"
  chmod 644 "$UNIT_DIR/omdesky-agent.service"

  systemctl --user daemon-reload >/dev/null 2>&1 || true
}

if [ -n "$LOCAL_DIR" ]; then
  [ -d "$LOCAL_DIR" ] || err "$LOCAL_DIR is not a directory"
  info "Installing from $LOCAL_DIR"
  install_from_dir "$(cd "$LOCAL_DIR" && pwd)"
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

  expected="${OMDESKY_SHA256:-}"

  if [ -z "$expected" ]; then
    info "Downloading the published checksum"
    download "$url.sha256" "$workdir/omdesky.tar.gz.sha256"
    expected="$(cut -d' ' -f1 "$workdir/omdesky.tar.gz.sha256")"
    warn "the checksum was published alongside the archive; set OMDESKY_SHA256 to pin a checksum you obtained independently"
  fi

  verify_checksum "$workdir/omdesky.tar.gz" "$expected"
  verify_archive_layout "$workdir/omdesky.tar.gz" "$asset"

  tar -xzf "$workdir/omdesky.tar.gz" -C "$workdir"

  install_from_dir "$workdir/$asset"
fi

if ! printf '%s' ":$PATH:" | grep -q ":$BIN_DIR:"; then
  warn "$BIN_DIR is not in your PATH; add it so you can run omdesky"
fi

info "Installed. Next steps on the computer you want to control:"
printf '  omdesky setup\n'
printf '  omdesky access allow <controller>\n'
printf '  systemctl --user enable --now omdesky-agent.service\n'
printf '  omdesky doctor\n'
