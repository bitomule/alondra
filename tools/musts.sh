#!/bin/sh
# `bazelisk run //tools:musts -- <arguments>`: the pinned musts, run from the checkout's root.
set -e
musts="$(pwd)/$1"
shift
cd "${BUILD_WORKSPACE_DIRECTORY:?run this with bazelisk run //tools:musts}"
exec "$musts" "$@"
