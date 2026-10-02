#!/bin/sh
# Usage: tools/e2e-sandbox.sh <alondrad binary> <alondra binary>
# Starts a throwaway alondrad (own data dir and port, sandbox mode, wake off) in the foreground
# and a copy of the node plugin that talks only to it. Ctrl-C stops it.
set -eu

daemon=$1
cli=$2
port=${SANDBOX_PORT:-47891}
dir=${SANDBOX_DIR:-/tmp/alondra-sandbox-$$}
repo=$(cd "$(dirname "$0")/.." && pwd)

if [ "$port" = 47800 ]; then
	echo "47800 is the real alondrad's port; pick another SANDBOX_PORT." >&2
	exit 1
fi
if curl -s --max-time 2 "http://127.0.0.1:$port/v1/health" >/dev/null 2>&1; then
	echo "Something already answers on port $port; stop it or pick another SANDBOX_PORT." >&2
	exit 1
fi

# Binaries are replaced by rename, never over a running one. The daemon itself keeps $dir/plugin
# up to date from this checkout's plugin, in place, as it does from the marketplace in production.
mkdir -p "$dir/bin"
for bin in alondrad alondra; do
	source_bin=$daemon
	[ "$bin" = alondra ] && source_bin=$cli
	cp "$source_bin" "$dir/bin/$bin.next"
	chmod 755 "$dir/bin/$bin.next"
	mv -f "$dir/bin/$bin.next" "$dir/bin/$bin"
done
printf 'port = %s\n' "$port" >"$dir/config.toml"
printf '{"enabledPlugins": {"alondra@alondra": false, "alondra-orchestrator@alondra": false}, "env": {"ALONDRA_LAUNCHED": "1"}}\n' >"$dir/settings.json"
printf 'export ALONDRA_HOME="%s"\nexport ALONDRA_PORT="%s"\n' "$dir" "$port" >"$dir/env"

cat <<EOF
Alondra sandbox on http://127.0.0.1:$port, data in $dir (sandbox mode: test/ nodes allowed, wake off).

Launch a probe node against it (any installed plugin is switched off for that session, the
sandbox copy of the plugin takes its place, and the probe carries the mark Alondra's own
launches carry, without which the sandbox would leave it out):

  claude --bg --name test/<probe> --plugin-dir $dir/plugin --settings $dir/settings.json -- "<prompt>"

See its inbox, or run the CLI against it:

  . $dir/env && $dir/bin/alondra

Close the probe with 'claude rm <id>' when done, then Ctrl-C here.
EOF

ALONDRA_HOME="$dir" ALONDRA_PORT="$port" ALONDRA_SANDBOX=1 ALONDRA_WAKE=0 ALONDRA_PLUGIN_SOURCE="$repo/plugin" exec "$dir/bin/alondrad"
