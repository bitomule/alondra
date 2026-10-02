#!/bin/sh
# Runs the node mod's tests (plugin/tests/*.test.ts) with `claude plugin test`, the Claude Code
# on PATH, on a copy of the plugin and with a throwaway Claude config: no session, sign-in or
# network. Usage: tools/plugin-mod-test.sh <plugin/.claude-plugin/plugin.json>
set -eu

source_dir=$(dirname "$(dirname "$1")")
work=${TEST_TMPDIR:-$(mktemp -d)}
rm -rf "$work/plugin" "$work/claude"
cp -RL "$source_dir" "$work/plugin"
command -v claude >/dev/null || { echo "Claude Code (claude) is not on PATH" >&2; exit 1; }
HOME="$work" CLAUDE_CONFIG_DIR="$work/claude" claude plugin validate --strict "$work/plugin"
HOME="$work" CLAUDE_CONFIG_DIR="$work/claude" claude plugin test "$work/plugin"
