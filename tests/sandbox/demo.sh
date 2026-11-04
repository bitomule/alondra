#!/bin/sh
# The demo world has its invented projects and nothing of anyone's own.
set -eu

dir=$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/alondra-demo.XXXXXX")
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48150}" MACS=1 FAKES=1 SDK_NODE="$5" SDK_HOST="$6"
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey demo
python3 -I - "$dir/mac-1/domain.db" <<'PY'
import sqlite3
import sys

with sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True) as database:
    titles = {title for (title,) in database.execute("SELECT title FROM tasks")}
expected = {
    "2.4 release notes",
    "Publish 2.4",
    "Dark mode",
    "Offline mode",
    "Orders refresh rate",
}
assert titles == expected, f"the demo world has {titles}, not only its invented {expected}"
print(f"The demo world has its {len(expected)} invented tasks")
PY
