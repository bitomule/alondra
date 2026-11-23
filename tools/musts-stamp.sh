#!/bin/sh
# Bazel's workspace status command: stamps every build with the musts scope hashes. It uses the musts
# that Bazel downloaded (`tools/pinned_tools.bzl`), found in the output base the build's convenience
# link points to; the first build of a checkout has no link yet, so the default output base is
# derived from the checkout's path. A `musts` on PATH is the fallback. Always exits 0: a build never
# fails for lack of a stamp.
musts=
output_base=
if [ -L .bazel-links/bazel-out ]; then
  execroot=$(readlink .bazel-links/bazel-out)
  output_base=${execroot%/execroot/*}
elif command -v md5 >/dev/null 2>&1; then
  output_base=/private/var/tmp/_bazel_$(id -un)/$(md5 -q -s "$(pwd -P)")
fi
if [ -x "$output_base/external/+pinned_tools+musts/musts" ]; then
  musts="$output_base/external/+pinned_tools+musts/musts"
elif command -v musts >/dev/null 2>&1; then
  musts=musts
fi
[ -n "$musts" ] && "$musts" stamp
exit 0
