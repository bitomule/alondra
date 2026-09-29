#!/bin/sh
# Usage: tools/cutover-live.sh <alondrad built from this checkout>
# The one-command cutover of the live HOME to domain v2 (docs/domain-v2-live-cutover.md).
# It checks everything first, keeps a copy of the installed binaries, then waits for the
# running daemon to release HOME: upgrade the installed build and stop the old daemon while
# it waits. Once committed, it waits for the v2 daemon that launchd starts and runs doctor.
#   BACKUP_ROOT        where the backup directory is created (default: ~/alondra-cutover-backups)
#   LEGACY_DAEMON/CLI  the binaries to keep for rollback (default: the running daemon's and its alondra)
set -eu

daemon=$1
home=${ALONDRA_HOME:-$HOME/.alondra}
port=${ALONDRA_PORT:-47800}
root=${BACKUP_ROOT:-$HOME/alondra-cutover-backups}

fail() {
	echo "cutover-live: $*" >&2
	exit 1
}

"$daemon" --help | grep -q -- --cutover-domain-v2 || fail "$daemon has no --cutover-domain-v2"
[ -d "$home" ] || fail "$home does not exist"
home=$(realpath "$home")

pid=$(lsof -nP -t -iTCP:"$port" -sTCP:LISTEN 2>/dev/null | head -1 || true)
running=""
[ -n "$pid" ] && running=$(ps -o comm= -p "$pid" 2>/dev/null || true)
legacy_daemon=${LEGACY_DAEMON:-${running:-$(command -v alondrad || true)}}
[ -n "$legacy_daemon" ] && [ -f "$legacy_daemon" ] || fail "no legacy alondrad found; set LEGACY_DAEMON"
legacy_daemon=$(realpath "$legacy_daemon")
legacy_cli=${LEGACY_CLI:-$(dirname "$legacy_daemon")/alondra}
[ -f "$legacy_cli" ] || fail "no legacy alondra next to $legacy_daemon; set LEGACY_CLI"
legacy_cli=$(realpath "$legacy_cli")
echo "Legacy binaries kept for rollback: $("$legacy_daemon" --version) ($legacy_daemon), $legacy_cli"

if [ -f "$home/domain-v2-cutover.json" ]; then
	echo "$home already carries a cutover marker; the command will only report or resume."
else
	[ -f "$home/alondra.db" ] || fail "$home has no legacy store to cut over"
fi

mkdir -p "$root"
chmod 700 "$root"
root=$(realpath "$root")
case "$root/" in
"$home"/*) fail "BACKUP_ROOT must be outside $home" ;;
esac
backup="$root/backup-$(date +%Y%m%d-%H%M%S)"
[ -e "$backup" ] && fail "$backup already exists"

used_kb=$(du -sk "$home" | cut -f1)
free_kb=$(df -k "$root" | awk 'NR==2 {print $4}')
[ "$free_kb" -gt $((used_kb * 3)) ] || fail "$root has $((free_kb / 1024)) MB free; about $((used_kb * 3 / 1024)) MB are needed"

kept=$(mktemp -d "$root/legacy-bin.XXXXXX")
cp "$legacy_daemon" "$kept/alondrad"
cp "$legacy_cli" "$kept/alondra"

cat <<EOF
Ready: backup in $backup.
Now, in another terminal: upgrade the installed build to the v2 release, then stop the old daemon
(kill -TERM ${pid:-<its pid>}). launchd restarts the new one, which refuses the legacy HOME without
holding it; this command then takes HOME and cuts it over.
EOF

"$daemon" --cutover-domain-v2 "$backup" "$kept/alondrad" "$kept/alondra"
rm -rf "$kept"

echo "Waiting for the v2 daemon on port $port..."
waited=0
until curl -sf --max-time 2 "http://127.0.0.1:$port/v2/daemon" >/dev/null 2>&1; do
	[ "$waited" -lt 420 ] || fail "no v2 daemon on port $port after 420 s; check its log"
	sleep 2
	waited=$((waited + 2))
done
curl -sf "http://127.0.0.1:$port/v2/daemon"
echo
alondra doctor || true
