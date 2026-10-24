#!/bin/bash
# Bazel credential helper (https://github.com/EngFlow/credential-helper-spec) that sends the
# BuildBuddy API key from the macOS Keychain, so the key never appears among Bazel's options,
# which Bazel prints on a bad flag and writes to java.log on every server start.
#
#   ~/.bazelrc: common --credential_helper=remote.buildbuddy.io=<path to this script>
#   Store the key: security add-generic-password -U -a "$USER" -s buildbuddy-api-key -w
#
# The key is written only into the JSON response on stdout, never to stderr.
set -uo pipefail

service="${BUILDBUDDY_KEYCHAIN_SERVICE:-buildbuddy-api-key}"

if [ "$#" -ne 1 ] || [ "$1" != "get" ]; then
  echo "buildbuddy-credential-helper: only 'get' is supported" >&2
  exit 2
fi

cat >/dev/null

if ! key="$(security find-generic-password -s "$service" -a "$USER" -w 2>/dev/null)"; then
  echo "buildbuddy-credential-helper: no Keychain item '$service' for account '$USER'" >&2
  exit 1
fi

if ! [[ "$key" =~ ^[A-Za-z0-9]+$ ]]; then
  echo "buildbuddy-credential-helper: Keychain item '$service' is not a BuildBuddy API key" >&2
  exit 1
fi

printf '{"headers":{"x-buildbuddy-api-key":["%s"]}}\n' "$key"
