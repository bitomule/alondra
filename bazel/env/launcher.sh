#!/bin/bash
# alondra-bazel-env-launcher
# Installed into ~/.local/bin as musts and simpool by tools/bazel. Runs the tool pinned by the repo
# you are in, from its bazel/env/.bazel_env, and the next one on PATH outside such a repo.
set -euo pipefail

tool=$(basename "$0")
root=$(git rev-parse --show-toplevel 2>/dev/null || true)
if [ -n "$root" ] && [ -x "$root/bazel/env/ensure.sh" ]; then
	"$root/bazel/env/ensure.sh"
	if [ -x "$root/bazel/env/.bazel_env/bin/$tool" ]; then
		exec "$root/bazel/env/.bazel_env/bin/$tool" "$@"
	fi
fi

# Skip every copy of this launcher, or two of them on PATH would call each other forever.
IFS=: read -r -a path_dirs <<<"$PATH"
for dir in "${path_dirs[@]}"; do
	[ -n "$dir" ] && [ -x "$dir/$tool" ] || continue
	grep -q alondra-bazel-env-launcher "$dir/$tool" 2>/dev/null && continue
	exec "$dir/$tool" "$@"
done
echo "$tool: not in a repo that pins it, and no other $tool on PATH" >&2
exit 127
