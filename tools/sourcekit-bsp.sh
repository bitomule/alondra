#!/bin/bash
# SourceKit-LSP starts this through .bsp/skbsp.json, so it reads the apps' Bazel flags instead of
# building the root Package.swift with SwiftPM into a 1.9 GB .build. The server binary is built
# once per version into a cache every worktree shares; stdout belongs to the protocol.
set -euo pipefail

version=$(sed -n 's/^bazel_dep(name = "sourcekit_bazel_bsp", version = "\([^"]*\)".*/\1/p' MODULE.bazel)
binary="${SOURCEKIT_BSP_CACHE:-$HOME/Library/Caches/sourcekit-bazel-bsp}/$version/sourcekit-bazel-bsp"
target=@sourcekit_bazel_bsp//Sources:sourcekit-bazel-bsp

if [ ! -x "$binary" ]; then
    bazelisk build "$target" >&2
    built=$(bazelisk cquery --output=files "$target" 2>/dev/null)
    mkdir -p "$(dirname "$binary")"
    cp "$built" "$binary.$$"
    chmod 755 "$binary.$$"
    mv -f "$binary.$$" "$binary"
fi

exec "$binary" "$@"
