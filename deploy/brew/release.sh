#!/usr/bin/env bash
# Run once from a clean, merged main checkout; CI owns every publishing step.
set -euo pipefail
cd "$(dirname "$0")/../.."
[[ $# == 1 ]] || { echo 'usage: deploy/brew/release.sh MAJOR.MINOR.PATCH' >&2; exit 2; }
version=$1
python3 - "$version" <<'PY'
import sys, tomllib
sys.path.insert(0, 'deploy/brew')
from release import version
value = version(sys.argv[1])
with open('Cargo.toml', 'rb') as f:
    assert tomllib.load(f)['workspace']['package']['version'] == value, 'Bump Cargo version in a reviewed PR first'
PY
[[ $(git branch --show-current) == main ]] || { echo 'Release from main only' >&2; exit 1; }
[[ -z $(git status --porcelain) ]] || { echo 'Working tree must be clean' >&2; exit 1; }
git fetch origin main
[[ $(git rev-parse HEAD) == "$(git rev-parse origin/main)" ]] || { echo 'main must equal origin/main' >&2; exit 1; }
tag="alondra-v$version"
[[ -z $(git ls-remote origin "refs/tags/$tag") ]] || { echo 'Tag exists; retry failed workflow jobs instead' >&2; exit 1; }
git tag -a "$tag" -m "Alondra $version"
git push origin "refs/tags/$tag"
echo "Release queued. Follow: gh run list --workflow brew-release.yml"
