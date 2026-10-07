#!/usr/bin/env bash
# `make increment-version` in a throwaway repo: a refused commit must leave no release tag.
set -euo pipefail

makefile=$(cd "$(dirname "$1")" && pwd)/$(basename "$1")
shared=$(cd "$(dirname "$2")" && pwd)/$(basename "$2")
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

repo() {
  local dir="$work/$1"
  mkdir -p "$dir/tools" "$dir/deploy/apple"
  cp "$makefile" "$dir/Makefile"
  cp "$shared" "$dir/tools/shared.bzl"
  echo "import sys; sys.exit(0)" > "$dir/deploy/apple/version_guard.py"
  git -C "$dir" init -q
  git -C "$dir" config user.email test@example.com
  git -C "$dir" config user.name test
  git -C "$dir" config core.hooksPath .git/hooks
  git -C "$dir" config commit.gpgsign false
  git -C "$dir" config tag.gpgsign false
  git -C "$dir" add -A
  git -C "$dir" commit -qm base
  echo "$dir"
}

build=$(sed -n 's/^ *build_number = "\(.*\)".*/\1/p' "$shared")
version=$(sed -n 's/^ *version = "\(.*\)".*/\1/p' "$shared")
tag="v$version-$((build + 1))"

accepted=$(repo accepted)
make -s -C "$accepted" increment-version >/dev/null
git -C "$accepted" rev-parse -q --verify "refs/tags/$tag" >/dev/null \
  || { echo "FAIL: a successful commit did not create $tag"; exit 1; }
[[ "$(git -C "$accepted" log -1 --pretty=%s)" == "🔖 Release $version ($((build + 1)))" ]] \
  || { echo "FAIL: unexpected release commit subject"; exit 1; }

refused=$(repo refused)
printf '#!/bin/sh\nexit 1\n' > "$refused/.git/hooks/pre-commit"
chmod +x "$refused/.git/hooks/pre-commit"
if make -s -C "$refused" increment-version >/dev/null 2>&1; then
  echo "FAIL: increment-version succeeded although the commit was refused"; exit 1
fi
if git -C "$refused" rev-parse -q --verify "refs/tags/$tag" >/dev/null; then
  echo "FAIL: a refused commit still created $tag"; exit 1
fi
[[ "$(git -C "$refused" log -1 --pretty=%s)" == "base" ]] \
  || { echo "FAIL: a release commit exists although the hook refused it"; exit 1; }

echo "PASS"
