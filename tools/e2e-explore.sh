#!/bin/sh
# Usage: tools/e2e-explore.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# The tester agent: a fresh two-Mac sandbox with the fakes and a seeded world, the TUI of each Mac
# in tmux, and a print-mode Claude that uses them as a person would (tools/e2e/explorer.md) and
# writes report.md. MODEL (opus), BUDGET (USD, 5) and FOCUS (extra instructions) tune it;
# REPORT_DIR moves the report, KEEP=1 keeps the sandbox. Nothing here reaches the real Alondra.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
stamp=$(date +%Y%m%d-%H%M%S)
dir=${SANDBOX_DIR:-/tmp/alondra-explore-$stamp}
report=${REPORT_DIR:-/tmp/alondra-explore-$stamp-report}
session="alondra-explore-$stamp"
model=${MODEL:-opus}
budget=${BUDGET:-5}
command -v tmux >/dev/null || { echo "The tester drives the TUI through tmux; install it first." >&2; exit 1; }
command -v claude >/dev/null || { echo "The tester is a Claude Code run; claude is not on PATH." >&2; exit 1; }

mkdir -p "$report/screens"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48130}" MACS=2 FAKES=1
trap 'tmux kill-session -t "$session" 2>/dev/null || true; "$repo/tools/e2e-sandbox-stop.sh" "$dir" >/dev/null' EXIT

"$repo/tools/e2e-sandbox.sh" "$@" >"$report/sandbox.txt"
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey seed >"$report/seed.txt"
for mac in 1 2; do
	tui="env ALONDRA_HOME=$dir/mac-$mac ALONDRA_LANG=es TERM=xterm-256color $dir/bin/alondra"
	if [ "$mac" = 1 ]; then
		tmux new-session -d -s "$session" -n mac1 -x 160 -y 45 "$tui"
	else
		tmux new-window -t "$session" -n "mac$mac" "$tui"
	fi
done

tool="$dir/bin/alondra-sandbox --sandbox $dir"
brief="Sandbox: $dir. tmux session: $session (windows mac1 and mac2).
Sandbox tool: $tool (fake nodes: '$tool fake nodes', '$tool fake node <task> <verb>'; views: '$tool phone --name seed --mac N request GET <path>').
Each Mac's CLI: env ALONDRA_HOME=$dir/mac-N $dir/bin/alondra.
Report folder: $report (your final answer becomes report.md; captures go to screens/).
${FOCUS:-}"

# A clean environment: the tester must never inherit an Alondra node's variables or plugins.
env -i HOME="$HOME" USER="${USER:-}" PATH="$PATH" LANG="${LANG:-en_US.UTF-8}" TMPDIR="${TMPDIR:-/tmp}" TERM=xterm-256color \
	claude -p --model "$model" --max-budget-usd "$budget" --permission-mode dontAsk \
	--settings '{"enabledPlugins": {"alondra@alondra": false, "alondra-orchestrator@alondra": false}}' \
	--strict-mcp-config --mcp-config '{"mcpServers": {}}' \
	--allowedTools "Bash(tmux:*)" "Bash(sleep:*)" "Bash($dir/bin/alondra-sandbox:*)" "Bash(env ALONDRA_HOME=$dir/*)" \
	"Read" \
	--append-system-prompt "$(cat "$repo/tools/e2e/explorer.md")" \
	--output-format stream-json --verbose \
	"$brief" >"$report/transcript.jsonl" || echo "The tester run ended with an error; see $report/transcript.jsonl" >&2

# The tester has no file-writing tool: its final answer is the report.
python3 -c 'import json,sys
for line in open(sys.argv[1]):
    try: event = json.loads(line)
    except ValueError: continue
    if event.get("type") == "result" and event.get("result"): open(sys.argv[2], "w").write(event["result"])' "$report/transcript.jsonl" "$report/report.md"
cost=$(sed -n 's/.*"total_cost_usd":\([0-9.]*\).*/\1/p' "$report/transcript.jsonl" | tail -1)
echo "Report: $report/report.md (cost: \$${cost:-?})"
[ -f "$report/report.md" ] || { echo "The tester wrote no report." >&2; exit 1; }
