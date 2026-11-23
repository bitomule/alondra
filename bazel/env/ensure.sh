#!/bin/bash
# Builds bazel/env/.bazel_env when it is missing or out of date with MODULE.bazel or this package.
# Silent when it is current, so it is cheap enough to run before every tool call and Bazel command.
set -euo pipefail

ENV_DIR="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$ENV_DIR/../.." && pwd)"
STAMP="$ENV_DIR/.bazel_env.stamp"

# What .bazel_env is built from. Contents rather than modification times, which a checkout of
# another branch can move either way.
fingerprint() {
	cat "$ROOT/MODULE.bazel" "$ENV_DIR/BUILD.bazel" "$ENV_DIR/tools.bzl" | shasum | cut -c1-40
}

current() {
	[ -d "$ENV_DIR/.bazel_env/bin" ] && [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$(fingerprint)" ]
}

if ! current; then
	bazel=$(command -v bazelisk || command -v bazel || true)
	if [ -z "$bazel" ]; then
		echo "bazel/env: no bazelisk or bazel on PATH to build the pinned tools" >&2
		exit 1
	fi
	echo "bazel/env: updating the pinned tools (bazel run //bazel/env:bazel_env)" >&2
	# tools/bazel runs this script too; the variable stops it from doing so for this build.
	(cd "$ROOT" && ALONDRA_BAZEL_ENV_ENSURING=1 "$bazel" run //bazel/env:bazel_env -- update-symlink) >&2
	fingerprint >"$STAMP"
fi
