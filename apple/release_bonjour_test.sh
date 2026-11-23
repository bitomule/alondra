#!/bin/bash
# A release build browses only the real Alondra service. The e2e sandbox's service type comes in
# only through `--define=alondra_sandbox=1` (apple/Bonjour/BonjourSandbox.plist).
# Usage: release_bonjour_test.sh <release Bonjour.plist> <app Info.plist>...
# Reads the files with the shell itself: the test runs no program of its own, so no TEST_EXECS.
set -u
release=$1
shift
status=0
contains() {
    local text
    text=$(<"$2")
    [[ $text == *"$1"* ]]
}
if ! contains '<string>_alondra._tcp</string>' "$release"; then
    echo "$release does not list _alondra._tcp" >&2
    status=1
fi
for plist in "$release" "$@"; do
    if contains '_alondra-sandbox' "$plist"; then
        echo "$plist lists the sandbox's Bonjour service, which a release must not browse" >&2
        status=1
    fi
done
exit $status
