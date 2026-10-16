#!/bin/sh
# Usage: journeys.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# Starts a fresh two-Mac sandbox with the fake claude and gh, runs the pairing scenarios and then
# the scripted journeys (JOURNEY=task-cycle|local|session|reopen, all by default), and stops it.
# KEEP=1 keeps the sandbox directory for a look afterwards.
set -eu

dir=$(mktemp -d /tmp/alondra-jrn.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48030}" MACS=2 FAKES=1
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" scenario
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey ${JOURNEY:-all}
