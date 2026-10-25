#!/bin/sh
# Usage: tools/tui-sandbox.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
#        tools/tui-sandbox.sh --stop
# The TUI to review a change by hand: a fresh one-Mac sandbox with the fakes (tools/e2e-sandbox.sh),
# seeded with tasks and cards in every state a user meets (alondra-sandbox journey seed), and that
# Mac's TUI in the tmux session SESSION (alondra-tui), 160x45, in TUI_LANG (es).
# The TUI starts from an empty environment with a HOME and ALONDRA_HOME inside the sandbox, so
# nothing it does reaches the real ~/.alondra or its daemon. --stop ends the session and the sandbox.
set -eu

repo=$(cd "$(dirname "$0")/.." && pwd)
session=${SESSION:-alondra-tui}
dir=${SANDBOX_DIR:-$(git -C "$repo" rev-parse --show-toplevel)/.scratch/tui-$session}

if [ "${1:-}" = --stop ]; then
	tmux kill-session -t "=$session" 2>/dev/null || true
	"$repo/tools/e2e-sandbox-stop.sh" "$dir"
	exit 0
fi

command -v tmux >/dev/null || { echo "The TUI opens in tmux; install it first." >&2; exit 1; }
if tmux has-session -t "=$session" 2>/dev/null; then
	echo "The tmux session $session already exists; stop it with make tui-sandbox-stop SESSION=$session or pick another SESSION." >&2
	exit 1
fi

# A node's own ALONDRA_* variables name the real daemon; nothing here may inherit them.
for name in $(env | sed -n 's/^\(ALONDRA_[A-Za-z0-9_]*\)=.*/\1/p'); do
	unset "$name"
done
export SANDBOX_DIR="$dir" SANDBOX_PORT="${SANDBOX_PORT:-48210}" MACS=1 FAKES=1

"$repo/tools/e2e-sandbox.sh" "$@" >/dev/null
trap '"$repo/tools/e2e-sandbox-stop.sh" "$dir" >&2' EXIT
"$dir/bin/alondra-sandbox" --sandbox "$dir" journey seed

home=$(cd "$dir/mac-1" && pwd -P)
port=$(sed -n 's/^export ALONDRA_PORT="\(.*\)"$/\1/p' "$home/env")
mkdir -p "$dir/tui-home"
cat >"$dir/tui" <<EOF
#!/bin/sh
exec env -i HOME="$dir/tui-home" PATH=/usr/bin:/bin LANG=en_US.UTF-8 TERM=xterm-256color \\
	ALONDRA_HOME="$home" ALONDRA_PORT="$port" ALONDRA_SANDBOX=1 ALONDRA_LANG="${TUI_LANG:-es}" \\
	"$dir/bin/alondra" "\$@"
EOF
chmod 755 "$dir/tui"
tmux new-session -d -s "$session" -x 160 -y 45 "$dir/tui"
trap - EXIT

cat <<EOF

The TUI runs against the sandbox in $dir (daemon on http://127.0.0.1:$port), never the real ~/.alondra.
  Watch it:    tmux attach -t $session
  Capture it:  tmux capture-pane -p -t $session
  Type in it:  tmux send-keys -t $session <keys>
  Its CLI:     $dir/tui <command>
  Move nodes:  $dir/bin/alondra-sandbox --sandbox $dir fake nodes
Stop it with: make tui-sandbox-stop${SESSION:+ SESSION=$session}
EOF
