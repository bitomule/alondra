#!/bin/sh
# Usage: sdk_journey.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox> <node> <persistent.cjs> <native archive>
# Starts a fresh sandbox whose Mac runs its tasks through the Agent SDK host and the pinned native
# Claude (SDK=1), runs the sdk journey and stops it. KEEP=1 keeps the sandbox directory.
set -eu

dir=$(mktemp -d /private/tmp/alondra-sdk.XXXXXX)
rmdir "$dir"
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48130}" MACS=1 FAKES=1 SDK=1 SDK_NODE="$5" SDK_HOST="$6" SDK_ARCHIVE="$7"
trap 'tools/e2e-sandbox-stop.sh "$dir" >/dev/null' EXIT

tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey sdk

# The daemon stops its SDK hosts, with the Claude each runs, and waits for each to exit before it
# exits itself.
hosts() {
	ps -axo pid=,command= | awk -v node="$dir/sdk/runtime/node" -v claude="$dir/sdk/runtime/claude" '$2 == node || $2 == claude { print $1 }'
}
running=$(hosts)
detail=$(ps -axo pid=,ppid=,pgid=,command= | awk -v node="$dir/sdk/runtime/node" -v claude="$dir/sdk/runtime/claude" '$4 == node || $4 == claude')
[ -n "$running" ] || { echo "the sdk journey left no SDK host running to stop" >&2; exit 1; }
daemon=$(cat "$dir"/mac-1/alondrad.pid)
# Every exit is registered before the SIGTERM: the hosts must exit before the daemon does, not
# after it, when their lifeline would end them anyway.
python3 -I -c '
import os, select, signal, sys
daemon, hosts = int(sys.argv[1]), [int(pid) for pid in sys.argv[2:]]
queue = select.kqueue()
queue.control([select.kevent(pid, select.KQ_FILTER_PROC, select.KQ_EV_ADD | select.KQ_EV_ONESHOT, select.KQ_NOTE_EXIT) for pid in [daemon, *hosts]], 0, 0)
os.kill(daemon, signal.SIGTERM)
order = []
while len(order) < 1 + len(hosts):
    order += [event.ident for event in queue.control(None, 1 + len(hosts), None)]
late = order[order.index(daemon) + 1:]
if late:
    sys.exit(f"SDK host processes {late} outlived the daemon shutdown; exits in order {order}")
' "$daemon" $running
left=$(hosts)
[ -z "$left" ] || { echo "SDK host processes survived the daemon shutdown: $left; before it, with parent and group:" >&2; echo "$detail" >&2; exit 1; }
echo "The daemon stopped its SDK hosts (pids $(echo $running)) before exiting"
