#!/bin/sh
# Usage: sdk_journey.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox> <node> <persistent.cjs> <native archive>
# Starts a fresh sandbox whose Mac runs its tasks through the Agent SDK host and the pinned native
# Claude (SDK=1), runs the sdk journey and stops it. KEEP=1 keeps the sandbox directory.
set -eu

dir=$(mktemp -d /private/tmp/alondra-sdk.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48130}" MACS=1 FAKES=1 SDK=1 SDK_NODE="$5" SDK_HOST="$6" SDK_ARCHIVE="$7"
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey sdk
