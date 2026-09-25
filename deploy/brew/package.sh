#!/usr/bin/env bash
# Package already signed and notarized release products without publishing the private repo.
set -euo pipefail

if [[ $# -ne 5 ]]; then
  echo 'usage: package.sh VERSION ALONDRA_APP ALONDRA_CLI ALONDRAD OUTPUT_DIR' >&2
  exit 2
fi
version=$1
app=$2
cli=$3
daemon=$4
output_dir=$5
if [[ ! $version =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
  echo 'version must be numeric, e.g. 0.1' >&2
  exit 2
fi
repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)
[[ -d $app && -x $cli && -x $daemon ]] || {
  echo 'app, CLI and daemon must exist before packaging' >&2
  exit 2
}
codesign --verify --deep --strict "$app"
codesign --verify --strict "$cli"
codesign --verify --strict "$daemon"
xcrun stapler validate "$app"
spctl --assess --type execute "$app"

mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd -P)
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir -p "$stage/alondra/bin" "$stage/alondra/.claude-plugin"
ditto "$app" "$stage/alondra/Alondra.app"
ditto "$cli" "$stage/alondra/bin/alondra"
ditto "$daemon" "$stage/alondra/bin/alondrad"
ditto "$repo_root/.claude-plugin/marketplace.json" "$stage/alondra/.claude-plugin/marketplace.json"
cp -RP "$repo_root/plugin" "$repo_root/plugin-orchestrator" "$stage/alondra/"

archive="$output_dir/alondra-$version-macos.zip"
ditto -c -k --sequesterRsrc --keepParent "$stage/alondra" "$archive"
shasum -a 256 "$archive"
echo "Package ready. Upload and verify the archive, then fill the cask template with version, HTTPS URL and SHA-256."
