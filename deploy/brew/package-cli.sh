#!/usr/bin/env bash
# Package signed CLI, TUI, daemon and Claude assets for the public Homebrew formula.
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo 'usage: package-cli.sh VERSION TARGET ALONDRA_CLI ALONDRAD OUTPUT_DIR' >&2
  exit 2
fi
version=$1
target=$2
cli=$3
daemon=$4
output_dir=$5
if [[ ! $version =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
  echo 'version must be numeric, e.g. 0.1' >&2
  exit 2
fi
if [[ $target != aarch64-apple-darwin && $target != x86_64-apple-darwin ]]; then
  echo 'target must be a supported macOS Rust target' >&2
  exit 2
fi
repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)
for path in "$cli" "$daemon"; do
  [[ -f $path && -x $path ]] || { echo "missing executable: $path" >&2; exit 2; }
  codesign --verify --strict "$path"
  signature=$(codesign --display --verbose=4 "$path" 2>&1)
  [[ $signature == *'Authority=Developer ID Application:'* ]] || {
    echo "executable has no Developer ID Application signature: $path" >&2
    exit 2
  }
done
for path in "$repo_root/.claude-plugin/marketplace.json" "$repo_root/plugin" "$repo_root/plugin-orchestrator"; do
  [[ -e $path ]] || { echo "missing asset: $path" >&2; exit 2; }
done
mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd -P)
archive="$output_dir/alondra-$version-$target.tar.gz"
[[ ! -e $archive ]] || { echo "archive already exists: $archive" >&2; exit 2; }
stage=$(mktemp -d "$output_dir/.alondra-package.XXXXXXXX")
trap 'rm -rf "$stage"' EXIT
ditto "$cli" "$stage/alondra"
ditto "$daemon" "$stage/alondrad"
ditto "$repo_root/.claude-plugin" "$stage/.claude-plugin"
cp -RP "$repo_root/plugin" "$repo_root/plugin-orchestrator" "$stage/"
tar -C "$stage" -czf "$archive" alondra alondrad .claude-plugin plugin plugin-orchestrator
shasum -a 256 "$archive"
echo 'Verify notarization, upload both architecture archives to the tap release, then fill both formula checksums.'
