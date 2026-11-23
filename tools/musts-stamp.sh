#!/bin/sh
# Bazel's workspace status command: stamps every build with the musts scope hashes. It runs the musts
# that bazel/env pins, from bazel/env/.bazel_env, which tools/bazel builds before the build starts.
# It never calls the ~/.local/bin launcher: that may run Bazel, and this runs inside a Bazel command.
# A `musts` on PATH that is not a launcher is the fallback. Always exits 0: a build never fails for
# lack of a stamp.
musts=bazel/env/.bazel_env/bin/musts
if [ ! -x "$musts" ]; then
  musts=$(command -v musts 2>/dev/null)
  [ -n "$musts" ] && grep -q alondra-bazel-env-launcher "$musts" 2>/dev/null && musts=
fi
[ -n "$musts" ] && "$musts" stamp
exit 0
