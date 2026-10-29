#!/bin/sh
# A fresh TUI world has a working native session and measured simulated tokens.
set -eu

dir=$(mktemp -d "${TEST_TMPDIR:-${TMPDIR:-/tmp}}/alondra-seed.XXXXXX")
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48130}" MACS=1 FAKES=1 SDK_NODE="$5" SDK_HOST="$6"
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey seed
python3 -I - "$dir/mac-1/domain.db" <<'PY'
import sqlite3
import sys
import time

with sqlite3.connect(f"file:{sys.argv[1]}?mode=ro", uri=True) as database:
    deadline = time.monotonic() + 60
    while True:
        rows = database.execute(
            "SELECT s.session_id, s.input, s.output FROM usage_sessions s "
            "JOIN tasks t ON t.id = s.task_id AND t.agent_id = s.session_id "
            "WHERE t.id = 't-working' AND t.state = 'running'"
        ).fetchall()
        if any(input_tokens > 0 and output_tokens > 0 for _, input_tokens, output_tokens in rows):
            print(f"The seeded working task has measured native-session tokens: {rows}")
            break
        if time.monotonic() >= deadline:
            sys.exit(f"The seeded working task has no measured native-session tokens: {rows}")
        time.sleep(0.1)
PY
