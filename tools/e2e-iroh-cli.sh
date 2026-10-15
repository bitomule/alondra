#!/bin/sh
# Usage: tools/e2e-iroh-cli.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# A one-Mac sandbox whose relay announces a real iroh-relay (the image the Pi runs, in dev mode on
# loopback, asking the sandbox relay who may connect). The real alondra pairs directly, reads over
# iroh, then with only the iroh relay, and is shut out of it once revoked. Everything it started is
# removed when it ends, also on failure. IROH_RELAY_BIN=<iroh-relay 1.3.0 built with --features
# server> runs that binary instead of Docker.
set -eu

image=n0computer/iroh-relay:v1.3.0@sha256:7a51e3826531dba5eee73d8b48a8f27284684d98c4d0da271d99127cc43b9d57
base=${SANDBOX_PORT:-47990}
iroh_port=$((base + 60))
dir=$(mktemp -d /tmp/alondra-iroh-cli.XXXXXX)
config=$(mktemp -d /tmp/alondra-iroh-relay.XXXXXX)
container=alondra-iroh-relay-$$
rmdir "$dir"
token=$(openssl rand -hex 24)

cleanup() {
	status=$?
	if [ "$status" != 0 ]; then
		for log in "$config/iroh-relay.log" "$dir/relay.log" "$dir"/mac-1/*.log; do
			[ -f "$log" ] && { echo "--- $log" >&2; tail -25 "$log" >&2; }
		done
	fi
	[ -z "${relay_pid:-}" ] || kill "$relay_pid" 2>/dev/null || true
	[ -n "${IROH_RELAY_BIN:-}" ] || docker rm -f "$container" >/dev/null 2>&1 || true
	tools/e2e-sandbox-stop.sh "$dir" >/dev/null
	rm -rf "$config"
}
trap cleanup EXIT

if [ -n "${IROH_RELAY_BIN:-}" ]; then
	cat >"$config/relay.toml" <<TOML
enable_quic_addr_discovery = false
http_bind_addr = "127.0.0.1:$iroh_port"

[access.http]
url = "http://127.0.0.1:$base/v1/iroh/access"
TOML
	IROH_RELAY_HTTP_BEARER_TOKEN="$token" "$IROH_RELAY_BIN" --dev --config-path "$config/relay.toml" >"$config/iroh-relay.log" 2>&1 &
	relay_pid=$!
else
	cat >"$config/relay.toml" <<TOML
enable_quic_addr_discovery = false

[access.http]
url = "http://host.docker.internal:$base/v1/iroh/access"
TOML
	docker run -d --name "$container" -p "127.0.0.1:$iroh_port:3340" \
		-e IROH_RELAY_HTTP_BEARER_TOKEN="$token" -v "$config/relay.toml:/config/relay.toml:ro" \
		"$image" --dev --config-path /config/relay.toml >/dev/null
fi

export SANDBOX_DIR="$dir" SANDBOX_PORT="$base" MACS=1
export SANDBOX_IROH_RELAY="http://127.0.0.1:$iroh_port/" SANDBOX_IROH_TOKEN="$token"
tools/e2e-sandbox.sh "$1" "$2" "$3" "$4" >/dev/null
"$dir/bin/alondra-sandbox" --sandbox "$dir" iroh-cli
