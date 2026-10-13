#!/bin/sh
# Usage: allow_always.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# «Allow always» with the real Claude Code against a fresh sandbox; see tools/e2e_allow_always.py.
set -eu
exec python3 tools/e2e_allow_always.py "$@"
