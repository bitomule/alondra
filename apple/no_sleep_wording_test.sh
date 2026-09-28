#!/bin/bash
# Nodes are never "awake" or "asleep" to the user (see crates/alondra/tests/no_sleep_wording.rs
# for the TUI side). The app catalogs must not bring that distinction back.
set -u
status=0
for catalog in "$@"; do
    if grep -i -n -E 'awake|asleep|wakes up|wake up|dormant|sleeping|despiert|dormid' "$catalog"; then
        echo "$catalog mentions a node being awake or asleep, which the user should never see again" >&2
        status=1
    fi
done
exit $status
