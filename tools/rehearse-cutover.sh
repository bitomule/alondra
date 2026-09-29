#!/bin/sh
# Usage: tools/rehearse-cutover.sh <alondrad built from this checkout>
# Domain v2 cutover rehearsal (backup, archive, empty v2 start, rollback) against a
# read-only snapshot of the live HOME (docs/domain-v2-live-cutover.md). The live HOME and its daemon are never written,
# locked or stopped. Everything is copied into a new private directory, which is
# deleted at the end; only the report is kept.
#   LIVE_HOME       HOME to snapshot (default: $ALONDRA_HOME or ~/.alondra)
#   LEGACY          the old daemon retained for rollback (default: the installed alondrad)
#   REHEARSAL_ROOT  where the copies and the report go (default: /private/tmp)
set -eu

daemon=$1
live=${LIVE_HOME:-${ALONDRA_HOME:-$HOME/.alondra}}
legacy=${LEGACY:-$(command -v alondrad || true)}
root=${REHEARSAL_ROOT:-/private/tmp}

if [ -z "$legacy" ] || [ ! -f "$legacy" ]; then
	echo "No installed alondrad to retain as the legacy binary; set LEGACY=<path>." >&2
	exit 1
fi
legacy=$(realpath "$legacy")
live=$(realpath "$live")
root=$(realpath "$root")

case "$root/" in
"$live"/*)
	echo "REHEARSAL_ROOT must be outside $live." >&2
	exit 1
	;;
esac

used_kb=0
for item in alondra.db alondra.db-wal schedules.db schedules.db-wal domain-v2.db domain-v2.db-wal \
	service-receipts.sqlite service-receipts.sqlite-wal service-upload-attempts.sqlite \
	files candidates soak service-keys; do
	if [ -e "$live/$item" ]; then
		used_kb=$((used_kb + $(du -sk "$live/$item" | cut -f1)))
	fi
done
# Snapshot, backup and the restored HOME, plus headroom.
needed_kb=$((used_kb * 3 + 524288))
free_kb=$(df -k "$root" | awk 'NR==2 {print $4}')
if [ "$free_kb" -lt "$needed_kb" ]; then
	echo "$root has $((free_kb / 1024)) MB free; the rehearsal needs about $((needed_kb / 1024)) MB. Set REHEARSAL_ROOT." >&2
	exit 1
fi

reports="$root/alondra-cutover-reports"
mkdir -p "$reports"
chmod 700 "$reports"
scratch=$(mktemp -d "$root/alondra-cutover-rehearsal.XXXXXX")
trap 'rm -rf "$scratch"' EXIT INT TERM
report="$reports/rehearsal-$(date +%Y%m%d-%H%M%S)"

echo "Snapshot of $live into $scratch ($((needed_kb / 1024)) MB reserved, $((free_kb / 1024)) MB free); legacy binary $legacy"
status=0
"$daemon" --rehearse-domain-cutover "$live" "$scratch/work" "$legacy" "$report" || status=$?
echo "Report: $report.txt and $report.json"
exit "$status"
