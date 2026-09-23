#!/usr/bin/env bash
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
version="${1:-}"
changelog="${2:-$root/CHANGELOG.md}"
repo="${OMDESKY_REPO:-limahigor/omdesky}"

err() { printf 'release-notes: %s\n' "$1" >&2; exit 1; }

[ -n "$version" ] || err "usage: scripts/release-notes.sh <version> [changelog]"
[ -f "$changelog" ] || err "missing $changelog"

notes="$(awk -v version="$version" '
  /^## \[/ {
    headings++
    if (in_section) { in_section = 0; if (previous == "") previous = $0 }
    if (index($0, "## [" version "] - ") == 1) { in_section = 1; found = 1 }
  }
  headings == 0 && !/^## / { preamble = preamble $0 "\n"; next }
  in_section { section = section $0 "\n"; next }
  /^\[[^]]+\]: / {
    if (index($0, "[" version "]: ") == 1) links_started = 1
    if (links_started) links = links $0 "\n"
  }
  END {
    if (!found) exit 2
    sub(/\n+$/, "\n", section)
    printf "%s%s\n%s", preamble, section, links
    match(previous, /\[[^]]+\]/)
    printf "\n@@previous=%s\n", substr(previous, RSTART + 1, RLENGTH - 2)
  }
' "$changelog")" || err "CHANGELOG has no dated section for $version"

previous="$(sed -n 's/^@@previous=//p' <<<"$notes")"
body="$(sed '/^@@previous=/d' <<<"$notes")"

[ -n "$(awk '/^### /' <<<"$body")" ] || err "the $version section has no entries"

printf '%s\n\n' "$body"

if [ -n "$previous" ]; then
  printf '**Full Changelog**: https://github.com/%s/compare/v%s...v%s\n' "$repo" "$previous" "$version"
else
  printf '**Full Changelog**: https://github.com/%s/commits/v%s\n' "$repo" "$version"
fi
