#!/usr/bin/env bash
# Add the CLI, daemon and Claude plugins to a Mac app before Developer ID signing.
set -euo pipefail

if [[ $# -ne 4 ]]; then
  echo 'usage: assemble-app.sh INPUT_APP ALONDRA_CLI ALONDRAD OUTPUT_APP' >&2
  exit 2
fi
input_app=$1
cli=$2
daemon=$3
output_app=$4
repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)

[[ -d $input_app && -x $cli && -x $daemon ]] || {
  echo 'input app, CLI and daemon must exist' >&2
  exit 2
}
[[ ! -e $output_app ]] || {
  echo 'output app already exists' >&2
  exit 2
}

ditto "$input_app" "$output_app"
contents="$output_app/Contents"
assets="$contents/Resources/alondra"
mkdir -p "$contents/MacOS" "$assets/.claude-plugin"
ditto "$cli" "$contents/MacOS/alondra"
ditto "$daemon" "$contents/MacOS/alondrad"
ditto "$repo_root/.claude-plugin/marketplace.json" "$assets/.claude-plugin/marketplace.json"
cp -RP "$repo_root/plugin" "$repo_root/plugin-orchestrator" "$assets/"

echo 'App assembled. Sign both embedded binaries and the app, notarize, staple, then run package.sh.'
