#!/bin/bash
# Executed only through tart exec in the owned disposable clone.
set -euo pipefail
export PATH=/Users/admin/alondra-test-tools:/opt/homebrew/bin:/Users/admin/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin
ROOT=/Users/admin/alondra-gate
cd "$ROOT"
[[ "$(sysctl -n hw.model)" == VirtualMac* ]] || { echo 'Refusing non-virtual Mac'; exit 2; }
[[ -f /Users/admin/alondra-test-tools/base.json ]] || { echo 'Missing prepared-base marker'; exit 2; }
[[ ! -e "$HOME/.alondra" && ! -e "$HOME/Library/Containers/com.bitomule.alondra.mac" ]] || { echo 'Fixture contains Alondra user state'; exit 2; }
# Tart guest exec uses virtio sockets, so this does not disconnect control.
# No host network or permission changes; this is the disposable clone only.
sudo -n /sbin/ifconfig en0 down
interface_state=$(/sbin/ifconfig en0)
[[ "$interface_state" == en0:* ]] || { echo 'Invalid guest interface state'; exit 2; }
if printf '%s\n' "$interface_state" | head -1 | grep -Eq '<([^>]*,)?UP(,|>)'; then
  echo 'Guest network still up'; exit 2
fi
mkdir -p project/.mav
cp config.yaml project/.mav/config.yaml
cp demo.yaml project/demo.yaml
cd project
# The prepared base has permissions, but a fresh boot has no driver daemon.
open -n -g -a CuaDriver --args serve
for attempt in {1..20}; do
  mav doctor > doctor.txt
  if grep -q 'mac_permissions=ok' doctor.txt; then break; fi
  sleep 1
done
# doctor can succeed with missing optional tools; require the actual UI capabilities.
grep -q 'mac_permissions=ok' doctor.txt
grep -q 'mac_tree_driver=cua' doctor.txt
grep -q 'mac_semantic_tap_driver=cua' doctor.txt
codesign --verify --deep --strict ../Alondra.app
codesign -dvv ../Alondra.app 2> original-signature.txt
shasum -a 256 ../Alondra.app/Contents/MacOS/Alondra > original-executable.sha256
shasum -a 256 ../Alondra.app/Contents/MacOS/Alondra > runtime-executable.sha256
trap 'mav stop > stop.txt 2>&1 || true' EXIT
mav flow lint demo.yaml > flow-lint.txt
mav run demo.yaml --skip-build > flow.txt 2>&1
