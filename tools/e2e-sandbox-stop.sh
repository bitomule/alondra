#!/bin/sh
# Usage: tools/e2e-sandbox-stop.sh [sandbox dir]
# Stops the processes tools/e2e-sandbox.sh started there and deletes the directory (KEEP=1 keeps it).
# Only a PID whose command still belongs to that sandbox is signalled.
set -u

dir=${1:-${SANDBOX_DIR:-/tmp/alondra-sandbox}}

if [ ! -f "$dir/.alondra-sandbox" ]; then
	echo "No sandbox in $dir." >&2
	exit 0
fi

stop() {
	pid=$(cat "$1" 2>/dev/null) || return 0
	command=$(ps -p "$pid" -o command= 2>/dev/null) || { rm -f "$1"; return 0; }
	case $command in
	*"$dir"*) ;;
	*) rm -f "$1"; return 0 ;;
	esac
	kill "$pid" 2>/dev/null
	tries=0
	while kill -0 "$pid" 2>/dev/null && [ "$tries" -lt 50 ]; do
		sleep 0.2
		tries=$((tries + 1))
	done
	kill -9 "$pid" 2>/dev/null
	rm -f "$1"
}

for pidfile in "$dir"/mac-*/alondrad.pid; do
	[ -f "$pidfile" ] && stop "$pidfile"
done
stop "$dir/relay.pid"
stop "$dir/issuer.pid"

if [ "${KEEP:-0}" = 1 ]; then
	rm -f "$dir/sandbox.json"
	echo "Sandbox stopped; files kept in $dir."
else
	rm -rf "$dir"
	echo "Sandbox stopped and $dir removed."
fi
