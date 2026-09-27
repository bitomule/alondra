#!/bin/bash
# Opt-in CLI gate, copied into an owned disposable Tart guest by macos.py.
set -euo pipefail
umask 077
export PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
export HOMEBREW_NO_ANALYTICS=1 HOMEBREW_NO_AUTO_UPDATE=1 HOMEBREW_NO_INSTALL_CLEANUP=1
[[ $# == 1 && ( "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ || "$1" == lifecycle ) ]] || { echo 'Expected one release version or lifecycle'; exit 2; }
[[ "$(sysctl -n hw.model)" == VirtualMac* && "$(id -un)" == admin ]] || { echo 'Disposable VirtualMac/admin required'; exit 2; }
cd /Users/admin/alondra-gate
if [[ "$1" == lifecycle ]]; then exec python3 cli_lifecycle.py; fi
exec python3 cli.py "$1"
