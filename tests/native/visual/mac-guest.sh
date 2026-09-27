#!/bin/bash
# Runs only in the Gate-owned disposable clone; lifecycle belongs to macos.py.
set -euo pipefail
export PATH=/Users/admin/alondra-test-tools:/opt/homebrew/bin:/Users/admin/.local/bin:/usr/bin:/bin:/usr/sbin:/sbin
ROOT=/Users/admin/alondra-gate
cd "$ROOT"
[[ "$(sysctl -n hw.model)" == VirtualMac* ]] || exit 2
[[ -f /Users/admin/alondra-test-tools/base.json ]] || exit 2
[[ ! -e "$HOME/.alondra" && ! -e "$HOME/Library/Containers/com.bitomule.alondra.mac" ]] || exit 2
sudo -n /sbin/ifconfig en0 down
interface_state=$(/sbin/ifconfig en0)
[[ "$interface_state" == en0:* ]] || exit 2
if printf '%s\n' "$interface_state" | head -1 | grep -Eq '<([^>]*,)?UP(,|>)'; then exit 2; fi
mkdir -p driver-bin project/.mav
cp cua-session.py driver-bin/cua-driver
chmod 700 driver-bin/cua-driver
export PATH="$ROOT/driver-bin:$PATH"
cd project
open -n -g -a CuaDriver --args serve
for attempt in {1..20}; do
  mav doctor > doctor.txt
  if grep -q 'mac_permissions=ok' doctor.txt; then break; fi
  sleep 1
done
grep -q 'mac_permissions=ok' doctor.txt
grep -q 'mac_tree_driver=cua' doctor.txt
grep -q 'mac_semantic_tap_driver=cua' doctor.txt
codesign --verify --deep --strict ../Alondra.app
exec python3 ../mac_guest.py
