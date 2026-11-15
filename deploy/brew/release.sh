#!/usr/bin/env bash
# On-demand local automation. Signing keys stay in this Mac's keychain.
set -euo pipefail
if [ "${GITHUB_ACTIONS:-}" = true ]; then
    echo 'Releases must run locally, never inside GitHub Actions' >&2
    exit 1
fi
cd "$(dirname "$0")/../.."
exec python3 deploy/brew/local-release.py "$@"
