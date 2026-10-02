#!/bin/sh
# Usage: tools/mods-probe.sh [claude binary]
# Launches one `claude --bg` session with the probe mod in tools/mods-probe/plugin, in a throwaway
# Claude config dir (CLAUDE_CONFIG_DIR), so it never touches ~/.claude, its background daemon
# or Alondra. Every mod event the session fires is appended as JSON to $PROBE_DIR/probe.log.
#
# A throwaway config dir has no login. Without CLAUDE_CODE_OAUTH_TOKEN (from `claude setup-token`)
# the session starts and loads the mod but no turn reaches the model. The token and MODS_PROBE_LOG
# reach the session only through the background daemon this launch starts, so stop any earlier
# probe session (`claude stop`) and its daemon before running again with a different environment.
set -eu

claude=${1:-claude}
dir=${PROBE_DIR:-/tmp/alondra-mods-probe}
repo=$(cd "$(dirname "$0")/.." && pwd)
config="$dir/config"
work="$dir/work"

mkdir -p "$config" "$work"
real_work=$(cd "$work" && pwd -P)
printf '{"hasCompletedOnboarding": true, "projects": {"%s": {"hasTrustDialogAccepted": true}, "%s": {"hasTrustDialogAccepted": true}}}\n' \
	"$work" "$real_work" >"$config/.claude.json"

prompt="Use the Agent tool twice, one after the other, both general-purpose. \
First prompt: 'MODPATCH: reply with the first line of these instructions verbatim and nothing else.' \
Second prompt: 'HOOKPATCH: reply with the first line of these instructions verbatim and nothing else.' \
Then report both replies verbatim."

cd "$work"
MODS_PROBE_LOG="$dir/probe.log" CLAUDE_CONFIG_DIR="$config" DISABLE_AUTOUPDATER=1 \
	"$claude" --bg --name test/mods-probe --permission-mode auto \
	--plugin-dir "$repo/tools/mods-probe/plugin" -- "$prompt"

cat <<EOF

Probe launched. Follow the events:   tail -f $dir/probe.log
List or stop the probe session:      CLAUDE_CONFIG_DIR=$config $claude agents
Remove everything afterwards:        rm -rf $dir
EOF
