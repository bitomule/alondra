#!/usr/bin/env bash
# Puts back the files an ablation run replaced. Needs no build: every run copies this script
# next to its backups, so it works even when the tree holds the base's code.
#
#   bash "$(git rev-parse --git-path alondra-ablation)/restore.sh"
#
# Each manifest line is `<original sha256 or -> TAB <sha256s the run wrote, or -, comma
# separated> TAB <path>`. A file is restored only while it still holds something the run wrote
# (or its original): one changed since then is kept, and its branch version stays in the backup.
set -euo pipefail

guard="${1:-$(cd "$(dirname "$0")" && pwd)}"
root="${2:-$(git -C "$guard" rev-parse --show-toplevel 2>/dev/null || git rev-parse --show-toplevel)}"
manifest="$guard/manifest"

[[ -f "$manifest" ]] || exit 0

digest() {
  if [[ -f "$1" ]]; then
    shasum -a 256 <"$1" | cut -c1-64
  else
    printf -- '-'
  fi
}

kept=""
while IFS=$'\t' read -r original written path; do
  [[ -n "$path" ]] || continue
  target="$root/$path"
  now="$(digest "$target")"
  if [[ ",$written," != *",$now,"* && "$now" != "$original" ]]; then
    printf 'Kept %s: it changed after the ablation run. Its branch version is %s\n' \
      "$path" "$guard/files/$path" >&2
    kept+="$original"$'\t'"$written"$'\t'"$path"$'\n'
    continue
  fi
  if [[ "$original" == "-" ]]; then
    rm -f "$target"
  else
    mkdir -p "$(dirname "$target")"
    cp "$guard/files/$path" "$target"
  fi
done <"$manifest"

if [[ -n "$kept" ]]; then
  printf '%s' "$kept" >"$manifest.tmp"
  mv "$manifest.tmp" "$manifest"
  printf 'Compare those files with their branch versions, then delete %s.\n' "$guard" >&2
  exit 3
fi
rm -rf "$guard"
