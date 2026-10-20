#!/bin/sh
# Usage: allow_always.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox> <node> <persistent.cjs>
# «Allow always» against a fresh sandbox; see tools/e2e_allow_always.py.
set -eu
export SDK_NODE="$5" SDK_HOST="$6"
exec python3 tools/e2e_allow_always.py "$1" "$2" "$3" "$4"
