#!/bin/sh
# Usage: tools/e2e-iroh.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# Starts a one-Mac sandbox in a fresh directory, checks its iroh endpoint and stops it.
set -eu

dir=$(mktemp -d /tmp/alondra-iroh.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-47990}" MACS=1
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" iroh
