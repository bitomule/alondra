#!/bin/sh
# Usage: tools/e2e-sandbox.sh <alondrad> <alondra> <alondra-relay> <alondra-sandbox>
# Starts MACS throwaway alondrad daemons in the background, each with its own HOME, local port
# and Mac service keys, plus a loopback relay and token issuer they and the synthetic phone use.
# Each Mac also listens for direct connections on PORT+20+n and for iroh on UDP PORT+40+n, without
# a relay or port mapping (loopback; SANDBOX_LAN=1 opens both to the LAN to try a real phone), and
# advertises itself on Bonjour as _alondra-sandbox._tcp.
# SANDBOX_NAME names its Macs "<name> n" on Bonjour, so a test app picks its own sandbox.
# Sandbox mode: test/ nodes allowed, wake off (SANDBOX_WAKE=1 turns it on, for a fake claude on PATH).
# Stop everything with tools/e2e-sandbox-stop.sh.
# SDK_NODE and SDK_HOST name the pinned Node and the host bundle (//agent-host:pinned_node and
# //agent-host:persistent.cjs): with them every Mac gets the Agent SDK backend, whose native Claude
# is the sandbox's scripted fake `claude` (tools/sandbox/src/fake/native.rs), with a synthetic HOME
# and no model (tools/e2e_sdk_backend.py --fake). Every task and orchestrator batch runs through
# the SDK host.
# FAKES=1 (which needs SDK_NODE and SDK_HOST) puts the sandbox's own claude and gh first on the
# daemons' PATH: no model and no GitHub, a throwaway repository with a bare remote, and wake on, so
# the orchestrator and launches run for real against the fakes, with fast timing.
# NEW_MAC=1 adds one more Mac that nothing has set up, for `alondra`'s setup wizard: its daemon
# starts only when the wizard installs the service, through scripts that stand in for launchd
# and the relay account.
# SDK=1 (with FAKES=1, and SDK_ARCHIVE naming the native Claude's npm archive) gives Mac 1 the
# pinned native Claude instead: every task it launches runs through the SDK host and the real
# native Claude, under Seatbelt, against a local model double, with a synthetic HOME
# (tools/e2e_sdk_backend.py). Never the real login.
# SANDBOX_IROH_RELAY=<url> with SANDBOX_IROH_TOKEN makes the relay announce that iroh relay and
# answer its access check with that bearer, and the Macs follow it (tools/e2e-iroh-cli.sh).
set -eu

daemon=$1
cli=$2
relay_bin=$3
tool_bin=$4
macs=${MACS:-1}
fakes=${FAKES:-0}
repo=$(cd "$(dirname "$0")/.." && pwd)
dir=${SANDBOX_DIR:-$(git -C "$repo" rev-parse --show-toplevel)/.scratch/sandbox}
base=${SANDBOX_PORT:-47890}
issuer_port=$((base - 1))
relay_port=$base
direct_listen=127.0.0.1
[ "${SANDBOX_LAN:-0}" = 1 ] && direct_listen=0.0.0.0

case $macs in '' | *[!0-9]* | 0) echo "MACS must be a number from 1." >&2; exit 1 ;; esac
case $fakes in 0 | 1) ;; *) echo "FAKES is 0 or 1." >&2; exit 1 ;; esac
sdk=${SDK:-0}
sdk_backend=0
if [ -n "${SDK_NODE:-}" ] || [ -n "${SDK_HOST:-}" ]; then
	for artifact in "${SDK_NODE:?SDK_HOST needs SDK_NODE}" "${SDK_HOST:?SDK_NODE needs SDK_HOST}"; do
		[ -f "$artifact" ] || { echo "$artifact does not exist." >&2; exit 1; }
	done
	sdk_backend=1
fi
[ "$fakes" = 0 ] || [ "$sdk_backend" = 1 ] || { echo "FAKES=1 needs SDK_NODE and SDK_HOST: its tasks run through the Agent SDK host." >&2; exit 1; }
case $sdk in
0) ;;
1) [ "$fakes" = 1 ] || { echo "SDK=1 needs FAKES=1." >&2; exit 1; }
	[ -f "${SDK_ARCHIVE:?SDK=1 needs SDK_ARCHIVE}" ] || { echo "$SDK_ARCHIVE does not exist." >&2; exit 1; } ;;
*) echo "SDK is 0 or 1." >&2; exit 1 ;;
esac

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
iroh_port() { echo $((base + 40 + $1)); }

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
# A Unix socket's path fits in 104 bytes and a worktree's does not leave room, so the Macs' homes and
# the fakes, which hold sockets, live in a short folder of their own that $dir links to.
short=$(mktemp -d /private/tmp/asb.XXXXXX)
printf '%s\n' "$short" >"$dir/short-root"
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
if [ -n "${SANDBOX_IROH_RELAY:-}" ]; then
	(umask 077 && printf '%s' "${SANDBOX_IROH_TOKEN:?SANDBOX_IROH_RELAY needs SANDBOX_IROH_TOKEN}" >"$dir/iroh-access-token")
	export ALONDRA_IROH_RELAY_URL="$SANDBOX_IROH_RELAY" ALONDRA_IROH_ACCESS_TOKEN_FILE="$dir/iroh-access-token"
fi
ALONDRA_RELAY_CONFIG="$dir/relay.json" nohup "$dir/bin/alondra-relay" >"$dir/relay.log" 2>&1 &
echo $! >"$dir/relay.pid"
wait_until "The relay" "curl -sf ${relay}v1/bootstrap" "$dir/relay.log"

fake() {
	mkdir -p "$short/fakes"
	[ -L "$dir/fakes" ] || ln -s "$short/fakes" "$dir/fakes"
	printf '#!/bin/sh\nexec "%s" --sandbox "%s" %s -- "$@"\n' "$tool" "$dir" "$2" >"$dir/fakes/$1"
	chmod 755 "$dir/fakes/$1"
}
if [ "$sdk_backend" = 1 ]; then
	fake claude fake-claude
fi

daemon_path=$PATH
claude_home=
wake=${SANDBOX_WAKE:-0}
if [ "$fakes" = 1 ]; then
	mkdir -p "$dir/claude-home/plugins"
	fake claude fake-claude
	fake gh fake-gh
	# The marketplace sits in the sandbox so the plugin's bin/alondra wrapper finds $dir/bin/alondra.
	cp -RL "$repo/plugin" "$repo/plugin-orchestrator" "$dir/"
	printf '{"alondra": {"installLocation": "%s"}}\n' "$dir" >"$dir/claude-home/plugins/known_marketplaces.json"
	git init -q --bare -b main "$dir/remote.git"
	git init -q -b main "$dir/repo"
	printf '# Sandbox repository\n' >"$dir/repo/README.md"
	git -C "$dir/repo" add README.md
	git -C "$dir/repo" -c user.name="Alondra Sandbox" -c user.email=sandbox@alondra.invalid -c commit.gpgsign=false commit -q -m "Start the sandbox repository"
	git -C "$dir/repo" remote add origin "$dir/remote.git"
	git -C "$dir/repo" push -q -u origin main
	printf '{"projects": {"%s": {"hasTrustDialogAccepted": true}}}\n' "$(cd "$dir/repo" && pwd -P)" >"$dir/claude-home/.claude.json"
	daemon_path="$dir/fakes:$PATH"
	claude_home="$dir/claude-home"
	wake=1
fi

macs_json=
i=1
while [ "$i" -le "$macs" ]; do
	home="$short/mac-$i"
	port=$(local_port "$i")
	mkdir -p "$home"
	ln -s "$home" "$dir/mac-$i"
	mac_id=$("$tool" provision-mac "$home" --relay "$relay" --direct-port "$(direct_port "$i")" --direct-listen "$direct_listen" --iroh-port "$(iroh_port "$i")")
	"$tool" enroll-mac "$home" --relay "$relay" --issuer "$issuer" --key "$dir/issuer/key.der"
	[ -z "${SANDBOX_IROH_RELAY:-}" ] || sed -i '' 's/^relay = "off"$/relay = "auto"/' "$home/service.toml"
	printf 'port = %s\n' "$port" >"$home/config.toml"
	[ -z "${SANDBOX_NAME:-}" ] || printf 'name = "%s %s"\n' "$SANDBOX_NAME" "$i" >>"$home/config.toml"
	printf '{"enabledPlugins": {"alondra@alondra": false, "alondra-orchestrator@alondra": false}, "env": {"ALONDRA_LAUNCHED": "1"}}\n' >"$home/settings.json"
	printf 'export ALONDRA_HOME="%s"\nexport ALONDRA_PORT="%s"\n' "$home" "$port" >"$home/env"
	if [ "$sdk" = 1 ] && [ "$i" = 1 ]; then
		python3 -I "$repo/tools/e2e_sdk_backend.py" "$dir" "$home" "$SDK_NODE" "$SDK_HOST" "$SDK_ARCHIVE"
	elif [ "$sdk_backend" = 1 ]; then
		python3 -I "$repo/tools/e2e_sdk_backend.py" --fake "$dir" "$home" "$SDK_NODE" "$SDK_HOST" "$dir/fakes/claude"
	fi
	# start runs this Mac's daemon with its environment; a journey restarts the daemon with it too.
	{
		printf '#!/bin/sh\n'
		if [ "$fakes" = 1 ]; then
			printf 'export PATH="%s" CLAUDE_HOME="%s" CLAUDE_CONFIG_DIR="%s" ALONDRA_SANDBOX_TIMING=fast\n' "$daemon_path" "$claude_home" "$claude_home"
		fi
		printf 'export ALONDRA_HOME="%s" ALONDRA_PORT="%s" ALONDRA_SANDBOX=1 ALONDRA_WAKE=%s ALONDRA_PLUGIN_SOURCE="%s"\n' "$home" "$port" "$wake" "$repo/plugin"
		printf 'exec "%s"\n' "$dir/bin/alondrad"
	} >"$home/start"
	chmod 755 "$home/start"
	nohup "$home/start" >>"$home/alondrad.log" 2>&1 &
	echo $! >"$home/alondrad.pid"
	wait_until "Mac $i's alondrad" "curl -sf http://127.0.0.1:$port/daemon" "$home/alondrad.log"
	[ -n "$macs_json" ] && macs_json="$macs_json,"
	macs_json="$macs_json
    {\"number\": $i, \"mac_id\": \"$mac_id\", \"home\": \"$home\", \"port\": $port, \"direct_port\": $(direct_port "$i"), \"iroh_port\": $(iroh_port "$i")}"
	i=$((i + 1))
done

if [ "${NEW_MAC:-0}" = 1 ]; then
	home="$short/mac-new"
	port=$(local_port "$all")
	mkdir -p "$home/bin"
	ln -s "$home" "$dir/mac-new"
	cat >"$home/bin/service" <<SCRIPT
#!/bin/sh
# Stands in for launchd: gives the new Mac its port and a loopback direct listener, then
# (re)starts its alondrad.
set -eu
[ -f "$home/config.toml" ] || printf 'port = $port\n' >"$home/config.toml"
if [ -f "$home/service.toml" ] && ! grep -q '^\[direct\]' "$home/service.toml"; then
	printf '\n[direct]\nport = $(direct_port "$all")\nlisten = "$direct_listen"\n' >>"$home/service.toml"
fi
if [ -f "$home/service.toml" ] && ! grep -q '^\[iroh\]' "$home/service.toml"; then
	printf '\n[iroh]\nrelay = "off"\nport = $(iroh_port "$all")\nlisten = "$direct_listen"\nportmapper = false\n' >>"$home/service.toml"
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
    {\"number\": $all, \"mac_id\": \"\", \"home\": \"$home\", \"port\": $port, \"direct_port\": $(direct_port "$all"), \"iroh_port\": $(iroh_port "$all")}"
fi

trap - EXIT
cat >"$dir/sandbox.json" <<EOF
{
  "relay": "$relay",
  "issuer": "$issuer",
  "fakes": $( [ "$fakes" = 1 ] && echo true || echo false ),
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
Mac $i on http://127.0.0.1:$(local_port "$i"), direct listener on $direct_listen:$(direct_port "$i"), iroh on UDP $direct_listen:$(iroh_port "$i") (log: make e2e-sandbox-logs MAC=$i)
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

EOF
if [ "$sdk_backend" = 1 ]; then
	cat <<EOF
SDK backend: tasks and orchestrator batches run through the Agent SDK host, whose native Claude is
the sandbox's scripted claude (sessions in $dir/fakes/sessions).

EOF
fi
if [ "$sdk" = 1 ]; then
	cat <<EOF
Pinned native Claude on Mac 1: its tasks run through the Agent SDK and the pinned native Claude, against a
local model double (requests in $dir/sdk/model-requests.jsonl). Scripted journey:
  $tool --sandbox $dir journey sdk

EOF
fi
if [ "$fakes" = 1 ]; then
	cat <<EOF
Fakes: claude and gh are the sandbox's own (calls in $dir/fakes/calls.jsonl); nodes work in $dir/repo.
  Scripted journeys:  $tool --sandbox $dir journey all
  A node's next move: $tool --sandbox $dir fake node <task> work|ask|die|end

EOF
fi
cat <<EOF
Stop it with: make e2e-sandbox-stop
EOF
