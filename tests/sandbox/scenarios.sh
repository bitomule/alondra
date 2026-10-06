#!/bin/sh
# Usage: scenarios.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# Starts a two-Mac sandbox in a fresh directory, runs the synthetic phone's scenarios, stops it.
set -eu

dir=$(mktemp -d /tmp/alondra-sbx.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-47970}" MACS=2
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" scenario
