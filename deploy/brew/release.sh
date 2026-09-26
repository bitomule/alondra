#!/usr/bin/env bash
# On-demand local automation. Signing keys stay in this Mac's keychain.
set -euo pipefail
cd "$(dirname "$0")/../.."
exec python3 deploy/brew/local-release.py "$@"
