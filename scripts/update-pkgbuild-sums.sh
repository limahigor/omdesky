#!/usr/bin/env bash
# Pin packaging/arch/PKGBUILD-bin to the checksum of a published release tarball.
#
#   scripts/update-pkgbuild-sums.sh 0.1.2
#
# Run this after the release workflow has published the assets, then commit the
# result so the pinned digest is reviewable in git history.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pkgbuild="$root/packaging/arch/PKGBUILD-bin"
repo="${OMDESKY_REPO:-limahigor/omdesky}"
version="${1:-}"

err() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }
info() { printf '\033[1;34m==>\033[0m %s\n' "$1"; }

[ -n "$version" ] || err "usage: $(basename "$0") <version>"
command -v curl >/dev/null 2>&1 || err "curl is required"
command -v sha256sum >/dev/null 2>&1 || err "sha256sum is required"

asset="omdesky-${version}-x86_64-linux.tar.gz"
base="https://github.com/$repo/releases/download/v${version}"

workdir="$(mktemp -d)"
trap 'rm -rf "$workdir"' EXIT

info "Downloading $asset"
curl -fsSL "$base/$asset" -o "$workdir/$asset"
curl -fsSL "$base/$asset.sha256" -o "$workdir/$asset.sha256"

info "Verifying the published checksum file"
(cd "$workdir" && sha256sum --check --status "$asset.sha256") ||
  err "the published checksum does not match the published tarball"

digest="$(sha256sum "$workdir/$asset" | cut -d' ' -f1)"

info "Pinning $digest"
sed -i \
  -e "s/^pkgver=.*/pkgver=${version}/" \
  -e "s/^sha256sums=(.*)$/sha256sums=('${digest}')/" \
  "$pkgbuild"

"$root/scripts/check-pkgbuild-sums.sh" "$version"
