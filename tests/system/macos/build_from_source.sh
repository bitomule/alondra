#!/bin/bash
# Opt-in gate: a clean macOS with Xcode 27 builds and tests a clone with no private setup.
# Run it inside a disposable guest (or any Mac that has no ~/.bazelrc and ~/.alondra):
#   build_from_source.sh [--fresh-home] <git url or path> <full commit sha>
# --fresh-home runs the build under a new empty HOME, so a Mac with its own setup can run it.
# The checkout and that HOME are removed on exit, whatever happens, after stopping Bazel.
set -euo pipefail
export PATH=/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin
fresh_home=0
if [[ ${1:-} == --fresh-home ]]; then fresh_home=1; shift; fi
[[ $# == 2 && "$2" =~ ^[0-9a-f]{40}$ ]] || { echo 'Usage: build_from_source.sh [--fresh-home] <repository> <commit sha>'; exit 2; }
checkout=$(mktemp -d)
scratch_home=
cleanup() {
  status=$?
  trap - EXIT
  if [[ -f $checkout/alondra/MODULE.bazel ]] && command -v bazelisk >/dev/null; then
    (cd "$checkout/alondra" && bazelisk shutdown) >/dev/null 2>&1 || true
  fi
  for folder in "$checkout" "$scratch_home"; do
    [[ -n $folder && -d $folder ]] || continue
    chmod -R u+w "$folder"
    rm -rf "$folder"
  done
  exit "$status"
}
trap cleanup EXIT INT TERM
if (( fresh_home )); then
  scratch_home=$(mktemp -d)
  export HOME=$scratch_home
fi
echo "scratch: $checkout $scratch_home"
[[ ! -e "$HOME/.bazelrc" && ! -e "$HOME/.alondra" ]] || { echo 'Refusing a Mac with Alondra or Bazel user state; use --fresh-home'; exit 2; }
if [[ -z ${BUILD_FROM_SOURCE_SKIP_DISK_CHECK:-} ]]; then
  free_kb=$(df -k / | awk 'NR==2 {print $4}')
  (( free_kb >= 40 * 1024 * 1024 )) || { echo 'At least 40 GB free are needed'; exit 2; }
fi
command -v bazelisk >/dev/null || brew install bazelisk
git clone --quiet "$1" "$checkout/alondra"
cd "$checkout/alondra"
git checkout --quiet "$2"
xcodebuild -version
make build
make test
