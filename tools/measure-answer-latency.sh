#!/bin/sh
# A local iroh relay and the real Swift client/daemon fixtures, with per-stage timings.
set -eu

case "${1:-}" in
    ''|--validate) ;;
    *) echo "Usage: IROH_RELAY_BIN=/path/to/iroh-relay $0 [--validate]" >&2; exit 2 ;;
esac
: "${IROH_RELAY_BIN:?Set IROH_RELAY_BIN to the iroh-relay 1.3.0 executable}"
case "$IROH_RELAY_BIN" in
    /*) relay_bin=$IROH_RELAY_BIN ;;
    *) relay_bin=$PWD/$IROH_RELAY_BIN ;;
esac
repo=$(cd "$(dirname "$0")/.." && pwd)
cd "$repo"
mkdir -p .scratch
[ -f .scratch/.gitignore ] || printf '*\n' > .scratch/.gitignore
scratch=$(mktemp -d "$repo/.scratch/answer-latency.XXXXXX")
relay_pid=
cleanup() {
    result=$?
    if [ -n "$relay_pid" ]; then
        kill "$relay_pid" 2>/dev/null || true
        wait "$relay_pid" 2>/dev/null || true
    fi
    if [ "$result" -ne 0 ] && [ -f "$scratch/relay.log" ]; then
        cat "$scratch/relay.log" >&2
    fi
    chmod -R u+w "$scratch"
    rm -rf "$scratch"
    exit "$result"
}
trap cleanup EXIT
mkdir -p "$scratch/home"
version=$(HOME="$scratch/home" TMPDIR="$scratch" "$relay_bin" --version)
[ "$version" = 'iroh-relay 1.3.0' ] || { echo "Expected iroh-relay 1.3.0, got $version" >&2; exit 1; }
port=${ANSWER_LATENCY_PORT:-48370}
python3 -c 'import socket,sys; s=socket.socket(); s.bind(("127.0.0.1",int(sys.argv[1]))); s.close()' "$port"
printf 'enable_quic_addr_discovery = false\nhttp_bind_addr = "127.0.0.1:%s"\n' "$port" > "$scratch/relay.toml"
HOME="$scratch/home" TMPDIR="$scratch" RUST_LOG=iroh_relay=info \
    "$relay_bin" --dev --config-path "$scratch/relay.toml" > "$scratch/relay.log" 2>&1 &
relay_pid=$!
export ALONDRA_MEASURE_RELAY="http://127.0.0.1:$port/"
if [ "${1:-}" = --validate ]; then
    musts run bazel-test-root
else
    bazelisk test //apple/Data/Feed:ActionJournalProcessTests \
        --test_filter=measureAnswerThroughEachConnection --nocache_test_results --test_output=all
fi
