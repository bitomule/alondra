#!/usr/bin/env bash
# Zip an assembled, signed, stapled app for the public binary channel.
set -euo pipefail

if [[ $# -ne 3 ]]; then
  echo 'usage: package.sh VERSION ALONDRA_APP OUTPUT_DIR' >&2
  exit 2
fi
version=$1
app=$2
output_dir=$3
if [[ ! $version =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
  echo 'version must be numeric, e.g. 0.1' >&2
  exit 2
fi
for required in \
  "$app/Contents/MacOS/alondra" \
  "$app/Contents/MacOS/alondrad" \
  "$app/Contents/Resources/alondra/.claude-plugin/marketplace.json" \
  "$app/Contents/Resources/alondra/plugin" \
  "$app/Contents/Resources/alondra/plugin-orchestrator"; do
  [[ -e $required ]] || {
    echo "required app component missing: $required" >&2
    exit 2
  }
done
codesign --verify --deep --strict "$app"
codesign --verify --strict "$app/Contents/MacOS/alondra"
codesign --verify --strict "$app/Contents/MacOS/alondrad"
xcrun stapler validate "$app"
spctl --assess --type execute "$app"

mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd -P)
archive="$output_dir/alondra-$version-macos.zip"
ditto -c -k --sequesterRsrc --keepParent "$app" "$archive"
shasum -a 256 "$archive"
echo "Package ready. Upload as alondra-v$version to bitomule/homebrew-tap Releases, verify its SHA-256, then fill the cask template."
