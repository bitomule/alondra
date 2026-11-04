#!/bin/bash
# Wraps every `bazel test` (`test --run_under` in .bazelrc; the maintainer's ~/.bazelrc runs a
# copy, ~/bin/bazel-test-tmpdir): the test gets its own short TMPDIR, removed when it exits, fails
# or is cut, so tests that never clean their temp dirs cannot fill the disk. Short and under /tmp
# because macOS caps a unix socket path at 104 bytes.
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

child=
finish() {
    status=$1
    trap - EXIT INT TERM HUP
    if [ -n "$child" ]; then
        [ "$status" -gt 128 ] && kill -"$((status - 128))" "$child" 2>/dev/null
        wait "$child" 2>/dev/null
    fi
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
