#!/bin/sh
# Usage: journeys.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox> <node> <persistent.cjs>
# Starts a fresh two-Mac sandbox with the fake claude and gh, whose tasks and orchestrator batches
# run through the Agent SDK host with the sandbox's scripted Claude, runs the pairing scenarios and
# then the scripted journeys (JOURNEY=task-cycle|session|reopen|mcp|report|local|accounts, all by default), and stops it.
# KEEP=1 keeps the sandbox directory for a look afterwards.
set -eu

dir=$(mktemp -d /tmp/alondra-jrn.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48030}" MACS=2 FAKES=1 SDK_NODE="$5" SDK_HOST="$6"
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
python3 -I - "$dir/sandbox.json" <<'PY'
import json
import sys
import urllib.request

with open(sys.argv[1]) as manifest:
    macs = json.load(manifest)["macs"]
for mac in macs:
    url = f'http://127.0.0.1:{mac["port"]}/settings/accounts?capabilities=provider_accounts'
    with urllib.request.urlopen(url, timeout=60) as response:
        listing = json.load(response)
    if "claude" not in listing["config"].get("connected", []):
        sys.exit(f'Claude is not connected on sandbox Mac {mac["number"]}: {listing}')
    if not any(account["provider"] == "claude" and account["id"] == "default" and account["state"] == "signed_in"
               for account in listing["accounts"]):
        sys.exit(f'Claude is not signed in on sandbox Mac {mac["number"]}: {listing}')
    print(f'Claude is connected and signed in on sandbox Mac {mac["number"]}')
PY
"$dir/bin/alondra-sandbox" --sandbox "$dir" scenario
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey ${JOURNEY:-all}
