#!/bin/bash
# xcodebuild installs a test host over the copy a reused simulator already has as a delta patch, and
# Bazel gives every file in the bundle the same 1980 date: a resource whose size did not change
# counts as unchanged, so the simulator keeps the previous build's copy and a new translation of the
# same length never reaches the app. Removing the host first makes every run a full install.
set -euo pipefail
: "${SIMULATOR_UDID:?set by the test runner}" "${TEST_HOST_BUNDLE_ID:?set in the test target env}"
xcrun simctl uninstall "$SIMULATOR_UDID" "$TEST_HOST_BUNDLE_ID"
