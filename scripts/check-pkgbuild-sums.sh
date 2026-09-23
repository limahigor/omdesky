#!/usr/bin/env bash
# Reject a binary PKGBUILD that would install an unverified release artifact.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
pkgbuild="$root/packaging/arch/PKGBUILD-bin"
expected_version="${1:-}"

err() { printf '\033[1;31merror:\033[0m %s\n' "$1" >&2; exit 1; }

[ -f "$pkgbuild" ] || err "missing $pkgbuild"

sums="$(sed -n "s/^sha256sums=(\(.*\))$/\1/p" "$pkgbuild" | tr -d "'\"")"
[ -n "$sums" ] || err "PKGBUILD-bin does not declare sha256sums"

for sum in $sums; do
  case "$sum" in
    SKIP) err "PKGBUILD-bin uses sha256sums=('SKIP'); pin the released checksum instead" ;;
    *[!0-9a-f]*) err "PKGBUILD-bin checksum '$sum' is not a lowercase SHA-256 digest" ;;
    ?*) [ "${#sum}" -eq 64 ] || err "PKGBUILD-bin checksum '$sum' is not a SHA-256 digest" ;;
    *) err "PKGBUILD-bin checksum '$sum' is not a lowercase SHA-256 digest" ;;
  esac
done

if [ -n "$expected_version" ]; then
  declared="$(sed -n 's/^pkgver=\(.*\)$/\1/p' "$pkgbuild")"
  [ "$declared" = "$expected_version" ] ||
    err "PKGBUILD-bin pkgver is '$declared' but the release is '$expected_version'"
fi

printf 'PKGBUILD-bin pins a SHA-256 checksum for every source\n'
