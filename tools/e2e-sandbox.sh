#!/bin/sh
# Usage: tools/e2e-sandbox.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# Starts MACS throwaway alondrad daemons in the background, each with its own HOME, local port
# and Mac service keys, plus a loopback relay and token issuer they and the synthetic phone use.
# Each Mac also listens for direct connections on PORT+20+n (loopback; SANDBOX_LAN=1 opens it to
# the LAN to try a real phone) and advertises itself on Bonjour as _alondra-sandbox._tcp.
# Sandbox mode: test/ nodes allowed, wake off (SANDBOX_WAKE=1 turns it on, for a fake claude on PATH).
# Stop everything with tools/e2e-sandbox-stop.sh.
# NEW_MAC=1 adds one more Mac that nothing has set up, for `alondra`'s setup wizard: its daemon
# starts only when the wizard installs the service, through scripts that stand in for launchd
# and the relay account.
set -eu

daemon=$1
cli=$2
relay_bin=$3
tool_bin=$4
macs=${MACS:-1}
dir=${SANDBOX_DIR:-/tmp/alondra-sandbox}
base=${SANDBOX_PORT:-47890}
repo=$(cd "$(dirname "$0")/.." && pwd)
issuer_port=$((base - 1))
relay_port=$base
direct_listen=127.0.0.1
[ "${SANDBOX_LAN:-0}" = 1 ] && direct_listen=0.0.0.0

case $macs in '' | *[!0-9]* | 0) echo "MACS must be a number from 1." >&2; exit 1 ;; esac

if [ -e "$dir" ]; then
	echo "$dir already exists; stop that sandbox with make e2e-sandbox-stop, remove it, or pick another SANDBOX_DIR." >&2
	exit 1
fi

port_free() {
	case $1 in
	47800 | 47810)
		echo "$1 belongs to the real alondrad; pick another SANDBOX_PORT." >&2
		return 1
		;;
	esac
	if curl -s --max-time 2 "http://127.0.0.1:$1/daemon" 2>/dev/null | grep -q '"version"'; then
		echo "An alondrad already answers on port $1; stop it or pick another SANDBOX_PORT." >&2
		return 1
	fi
	if nc -z 127.0.0.1 "$1" 2>/dev/null; then
		echo "Something already listens on port $1; stop it or pick another SANDBOX_PORT." >&2
		return 1
	fi
}

local_port() { echo $((base + $1)); }
direct_port() { echo $((base + 20 + $1)); }

for port in "$issuer_port" "$relay_port"; do port_free "$port"; done
all=$macs
[ "${NEW_MAC:-0}" = 1 ] && all=$((macs + 1))
i=1
while [ "$i" -le "$all" ]; do
	port_free "$(local_port "$i")"
	port_free "$(direct_port "$i")"
	i=$((i + 1))
done

trap 'KEEP=1 "$repo/tools/e2e-sandbox-stop.sh" "$dir"; echo "Logs kept; remove $dir before the next start." >&2' EXIT

wait_until() {
	tries=0
	until eval "$2" >/dev/null 2>&1; do
		tries=$((tries + 1))
		if [ "$tries" -ge 120 ]; then
			echo "$1 did not start; see $3" >&2
			exit 1
		fi
		sleep 0.5
	done
}

# Each daemon keeps its own plugin copy up to date from this checkout's plugin, in place, as it
# does from the marketplace in production.
mkdir -p "$dir/bin" "$dir/issuer/public"
: >"$dir/.alondra-sandbox"
for pair in "alondrad:$daemon" "alondra:$cli" "alondra-relay:$relay_bin" "alondra-sandbox:$tool_bin"; do
	cp "${pair#*:}" "$dir/bin/${pair%%:*}"
	chmod 755 "$dir/bin/${pair%%:*}"
done
tool="$dir/bin/alondra-sandbox"
issuer="http://127.0.0.1:$issuer_port"
relay="http://127.0.0.1:$relay_port/"

openssl genpkey -algorithm RSA -pkeyopt rsa_keygen_bits:2048 -outform DER -out "$dir/issuer/key.der" 2>/dev/null
chmod 600 "$dir/issuer/key.der"
"$tool" jwks "$dir/issuer/key.der" "$dir/issuer/public/jwks.json"
nohup python3 -m http.server "$issuer_port" --bind 127.0.0.1 --directory "$dir/issuer/public" >"$dir/issuer.log" 2>&1 &
echo $! >"$dir/issuer.pid"
wait_until "The token issuer" "curl -sf $issuer/jwks.json" "$dir/issuer.log"

"$tool" relay-config --listen "127.0.0.1:$relay_port" --issuer "$issuer" --registry "$dir/relay-devices.json" "$dir/relay.json"
ALONDRA_RELAY_CONFIG="$dir/relay.json" nohup "$dir/bin/alondra-relay" >"$dir/relay.log" 2>&1 &
echo $! >"$dir/relay.pid"
wait_until "The relay" "curl -sf ${relay}v1/bootstrap" "$dir/relay.log"

macs_json=
i=1
while [ "$i" -le "$macs" ]; do
	home="$dir/mac-$i"
	port=$(local_port "$i")
	mkdir -p "$home"
	mac_id=$("$tool" provision-mac "$home" --relay "$relay" --direct-port "$(direct_port "$i")" --direct-listen "$direct_listen")
	"$tool" enroll-mac "$home" --relay "$relay" --issuer "$issuer" --key "$dir/issuer/key.der"
	printf 'port = %s\n' "$port" >"$home/config.toml"
	printf '{"enabledPlugins": {"alondra@alondra": false, "alondra-orchestrator@alondra": false}, "env": {"ALONDRA_LAUNCHED": "1"}}\n' >"$home/settings.json"
	printf 'export ALONDRA_HOME="%s"\nexport ALONDRA_PORT="%s"\n' "$home" "$port" >"$home/env"
	ALONDRA_HOME="$home" ALONDRA_PORT="$port" ALONDRA_SANDBOX=1 ALONDRA_WAKE="${SANDBOX_WAKE:-0}" ALONDRA_PLUGIN_SOURCE="$repo/plugin" \
		nohup "$dir/bin/alondrad" >"$home/alondrad.log" 2>&1 &
	echo $! >"$home/alondrad.pid"
	wait_until "Mac $i's alondrad" "curl -sf http://127.0.0.1:$port/daemon" "$home/alondrad.log"
	[ -n "$macs_json" ] && macs_json="$macs_json,"
	macs_json="$macs_json
    {\"number\": $i, \"mac_id\": \"$mac_id\", \"home\": \"$home\", \"port\": $port, \"direct_port\": $(direct_port "$i")}"
	i=$((i + 1))
done

if [ "${NEW_MAC:-0}" = 1 ]; then
	home="$dir/mac-new"
	port=$(local_port "$all")
	mkdir -p "$home/bin"
	cat >"$home/bin/service" <<SCRIPT
#!/bin/sh
# Stands in for launchd: gives the new Mac its port and a loopback direct listener, then
# (re)starts its alondrad.
set -eu
[ -f "$home/config.toml" ] || printf 'port = $port\n' >"$home/config.toml"
if [ -f "$home/service.toml" ] && ! grep -q '^\[direct\]' "$home/service.toml"; then
	printf '\n[direct]\nport = $(direct_port "$all")\nlisten = "$direct_listen"\n' >>"$home/service.toml"
fi
if [ -f "$home/alondrad.pid" ]; then
	pid=\$(cat "$home/alondrad.pid")
	kill "\$pid" 2>/dev/null || true
	while kill -0 "\$pid" 2>/dev/null; do sleep 0.2; done
fi
ALONDRA_HOME="$home" ALONDRA_PORT="$port" ALONDRA_SANDBOX=1 ALONDRA_WAKE=0 ALONDRA_PLUGIN_SOURCE="$repo/plugin" \
	nohup "$dir/bin/alondrad" >>"$home/alondrad.log" 2>&1 &
echo \$! >"$home/alondrad.pid"
SCRIPT
	cat >"$home/bin/enroll" <<SCRIPT
#!/bin/sh
# Stands in for the Alondra account: enrolls the new Mac with the sandbox relay.
set -eu
"$tool" enroll-mac "$home" --relay "$relay" --issuer "$issuer" --key "$dir/issuer/key.der"
{ printf 'relay_url = "$relay"\nowner_sub = "sandbox-owner"\n'; cat "$home/service.toml"; } >"$home/service.toml.relay"
mv "$home/service.toml.relay" "$home/service.toml"
SCRIPT
	chmod 755 "$home/bin/service" "$home/bin/enroll"
	printf 'export ALONDRA_HOME="%s"\nexport ALONDRA_PORT="%s"\nexport ALONDRA_SANDBOX=1\nexport ALONDRA_SANDBOX_SERVICE="%s"\nexport ALONDRA_SANDBOX_ENROLL="%s"\nexport ALONDRA_PLUGIN_SOURCE="%s"\n' \
		"$home" "$port" "$home/bin/service" "$home/bin/enroll" "$repo/plugin" >"$home/env"
	[ -n "$macs_json" ] && macs_json="$macs_json,"
	macs_json="$macs_json
    {\"number\": $all, \"mac_id\": \"\", \"home\": \"$home\", \"port\": $port, \"direct_port\": $(direct_port "$all")}"
fi

trap - EXIT
cat >"$dir/sandbox.json" <<EOF
{
  "relay": "$relay",
  "issuer": "$issuer",
  "macs": [$macs_json
  ]
}
EOF

cat <<EOF
Alondra sandbox in $dir: $macs Mac(s) in service mode, relay on $relay, all on loopback.

EOF
i=1
while [ "$i" -le "$macs" ]; do
	home="$dir/mac-$i"
	cat <<EOF
Mac $i on http://127.0.0.1:$(local_port "$i"), direct listener on $direct_listen:$(direct_port "$i") (log: make e2e-sandbox-logs MAC=$i)
  Probe node:  claude --bg --name test/<probe> --plugin-dir $home/plugin --settings $home/settings.json -- "<prompt>"
  Its CLI:     . $home/env && $dir/bin/alondra

EOF
	i=$((i + 1))
done
if [ "${NEW_MAC:-0}" = 1 ]; then
	cat <<EOF
New Mac $all, set up by nothing yet, for the setup wizard (run it from the folder it should propose):
  . $dir/mac-new/env && $dir/bin/alondra
  Its phone, approved in the wizard:  $tool --sandbox $dir phone --mac $all pair --direct --wait

EOF
fi
cat <<EOF
Synthetic phone (state in $dir/phones):
  $tool --sandbox $dir phone --mac 1 pair
  $tool --sandbox $dir phone --mac 1 request GET /capabilities
  $tool --sandbox $dir phone --mac 1 revoke
  $tool --sandbox $dir phone --mac 1 pair --direct
  $tool --sandbox $dir phone --mac 1 request --direct GET /capabilities
A terminal paired directly (pairing requests: . <Mac home>/env && $dir/bin/alondra device list):
  ALONDRA_HOME=$dir/client ALONDRA_SANDBOX=1 $dir/bin/alondra mac connect --host 127.0.0.1:$(direct_port 1)

Close probes with 'claude rm <id>' when done, then: make e2e-sandbox-stop
EOF
