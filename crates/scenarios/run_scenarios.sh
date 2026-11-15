#!/bin/bash
# Runs the scenario runner Bazel built; its arguments filter the cases by name.
set -euo pipefail
exec "${TEST_SRCDIR}/${TEST_WORKSPACE}/crates/scenarios/scenarios_runner" "$@"
