#!/bin/sh
# Usage: scenarios.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox> <node> <persistent.cjs>
# Starts a two-Mac sandbox in a fresh directory, each Mac with the Agent SDK backend, runs the
# synthetic phone's scenarios, stops it.
set -eu

dir=$(mktemp -d /tmp/alondra-sbx.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-47970}" MACS=2 SDK_NODE="$5" SDK_HOST="$6"
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT
trap 'exit 143' TERM
trap 'exit 130' INT
trap 'exit 129' HUP

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" scenario
