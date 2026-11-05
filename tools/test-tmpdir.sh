#!/bin/bash
# Wraps every `bazel test` (`test --run_under` in .bazelrc; the maintainer's ~/.bazelrc runs a
# copy, ~/bin/bazel-test-tmpdir): the test gets its own short TMPDIR, removed when it exits, fails
# or is cut, so tests that never clean their temp dirs cannot fill the disk. Short and under /tmp
# because macOS caps a unix socket path at 104 bytes. Whatever still runs from that folder when the
# test ends dies with its process group, and a passing test that left an Alondra host (a process
# whose stdin is a host.lock, the host's ownership lock) fails.
root=${BAZEL_TEST_TMPDIR_ROOT:-/tmp}

remove() {
    chmod -R u+w "$1" 2>/dev/null
    rm -rf "$1"
}

for stale in "$root"/bt.*; do
    owner=$(cat "$stale/.owner" 2>/dev/null) || continue
    case $owner in '' | *[!0-9]*) continue ;; esac
    kill -0 "$owner" 2>/dev/null || remove "$stale"
done

dir=$(mktemp -d "$root/bt.XXXXXX") || exec "$@"
echo $$ >"$dir/.owner"
export TMPDIR="$dir"

left_running() {
    PATH=$PATH:/usr/sbin lsof -a -u "$(id -u)" -d cwd,0 -Fpn 2>/dev/null |
        awk -v inside="$(cd "$dir" && pwd -P)/" '
            /^p/ { pid = substr($0, 2) }
            /^n/ && index(substr($0, 2), inside) == 1 { print pid, ($0 ~ /\/host\.lock$/ ? "host" : "process") }'
}

end_left_running() {
    hosts=0
    left=$(left_running)
    for pid in $(echo "$left" | awk '{ print $1 }' | sort -u); do
        if echo "$left" | grep -q "^$pid host$"; then
            hosts=$((hosts + 1))
            echo "The test left an Alondra host running: $(ps -o pid=,command= -p "$pid")" >&2
        fi
        if [ "$(ps -o pgid= -p "$pid" | tr -d ' ')" = "$pid" ]; then
            kill -KILL -- "-$pid" 2>/dev/null
        else
            kill -KILL "$pid" 2>/dev/null
        fi
    done
    [ "$hosts" = 0 ]
}

child=
finish() {
    status=$1
    trap - EXIT INT TERM HUP
    if [ -n "$child" ]; then
        [ "$status" -gt 128 ] && kill -"$((status - 128))" "$child" 2>/dev/null
        wait "$child" 2>/dev/null
    fi
    end_left_running || [ "$status" != 0 ] || status=1
    remove "$dir"
    exit "$status"
}
trap 'finish $?' EXIT
trap 'finish 129' HUP
trap 'finish 130' INT
trap 'finish 143' TERM

"$@" <&0 &
child=$!
wait "$child"
status=$?
child=
finish "$status"
