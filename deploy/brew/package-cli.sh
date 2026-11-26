#!/usr/bin/env bash
# Package the signed CLI, TUI, daemon, Agent SDK host and Claude assets for the public Homebrew formula.
set -euo pipefail

if [[ $# -ne 13 ]]; then
  echo 'usage: package-cli.sh VERSION TARGET ALONDRA_CLI ALONDRAD LAUNCHER SPEC_CLI HOST HOST_MANIFEST CODEX_HOST CODEX_MANIFEST ANTIGRAVITY_HOST ANTIGRAVITY_MANIFEST OUTPUT_DIR' >&2
  exit 2
fi
version=$1
target=$2
cli=$3
daemon=$4
launcher=$5
spec_cli=$6
host=$7
host_manifest=$8
codex_host=$9
codex_manifest=${10}
antigravity_host=${11}
antigravity_manifest=${12}
output_dir=${13}
if [[ ! $version =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]]; then
  echo 'version must be numeric, e.g. 0.1' >&2
  exit 2
fi
if [[ $target != aarch64-apple-darwin ]]; then
  echo 'target must be aarch64-apple-darwin; Alondra ships Apple Silicon only' >&2
  exit 2
fi
repo_root=$(cd "$(dirname "$0")/../.." && pwd -P)
for path in "$cli" "$daemon" "$launcher" "$spec_cli" "$host" "$codex_host" "$antigravity_host"; do
  [[ -f $path && -x $path ]] || { echo "missing executable: $path" >&2; exit 2; }
  codesign --verify --strict "$path"
  signature=$(codesign --display --verbose=4 "$path" 2>&1)
  [[ $signature == *'Authority=Developer ID Application:'* ]] || {
    echo "executable has no Developer ID Application signature: $path" >&2
    exit 2
  }
done
for path in "$repo_root/.claude-plugin/marketplace.json" "$repo_root/plugin" "$repo_root/plugin-orchestrator" "$repo_root/LICENSE.md" "$repo_root/THIRD_PARTY_NOTICES.md" "$repo_root/deploy/brew/node/LICENSE"; do
  [[ -e $path ]] || { echo "missing asset: $path" >&2; exit 2; }
done
mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd -P)
archive="$output_dir/alondra-$version-$target.tar.gz"
[[ ! -e $archive ]] || { echo "archive already exists: $archive" >&2; exit 2; }
stage=$(mktemp -d "$output_dir/.alondra-package.XXXXXXXX")
trap 'rm -rf "$stage"' EXIT
ditto "$cli" "$stage/alondra"
ditto "$daemon" "$stage/alondrad"
ditto "$launcher" "$stage/alondrad-launcher"
ditto "$spec_cli" "$stage/alondra-spec"
python3 "$repo_root/deploy/brew/release.py" stage-assets "$version" "$stage"
python3 "$repo_root/deploy/brew/release.py" stage-host "$version" "$stage" --host "$host" --host-manifest "$host_manifest"
python3 "$repo_root/deploy/brew/release.py" stage-codex-host "$version" "$stage" --host "$codex_host" --host-manifest "$codex_manifest"
python3 "$repo_root/deploy/brew/release.py" stage-antigravity-host "$version" "$stage" --host "$antigravity_host" --host-manifest "$antigravity_manifest"
COPYFILE_DISABLE=1 tar -C "$stage" -czf "$archive" alondra alondrad alondrad-launcher alondra-spec sdk codex antigravity .claude-plugin plugin plugin-orchestrator LICENSE.md THIRD_PARTY_NOTICES.md
shasum -a 256 "$archive"
python3 "$repo_root/deploy/brew/release.py" validate "$version" "$archive"
