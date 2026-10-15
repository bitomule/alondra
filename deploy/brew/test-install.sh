#!/usr/bin/env bash
# Disposable CI/test environment only: never replace an existing Alondra installation.
set -euo pipefail
# Validation must not update the host toolchain or link over an active runtime.
export HOMEBREW_NO_AUTO_UPDATE=1
version=$1
target=$2
archive=$(cd "$3" && pwd -P)/alondra-$version-$target.tar.gz
if brew list --formula --versions alondra >/dev/null 2>&1; then
  echo 'Alondra is already installed; use a clean test environment' >&2
  exit 1
fi
brew tap-new alondra/release-test
formula="$(brew --repository alondra/release-test)/Formula/alondra.rb"
python3 - "$version" "$archive" "$formula" <<'PY'
import hashlib, pathlib, re, sys
version, archive, output = sys.argv[1:]
text = pathlib.Path('deploy/brew/alondra.rb.in').read_text().replace('@VERSION@', version)
text = re.sub(r'  url ".*"', f'  url "{pathlib.Path(archive).as_uri()}"', text)
checksum = hashlib.sha256(pathlib.Path(archive).read_bytes()).hexdigest()
text = text.replace('@ARM_SHA256@', checksum)
pathlib.Path(output).write_text(text)
PY
trap 'brew uninstall --force alondra/release-test/alondra >/dev/null 2>&1 || true; brew untap alondra/release-test >/dev/null 2>&1 || true' EXIT
HOMEBREW_NO_INSTALL_CLEANUP=1 brew install --skip-link alondra/release-test/alondra
brew test --force alondra/release-test/alondra
prefix=$(brew --prefix alondra/release-test/alondra)
for binary in alondra alondrad alondrad-launcher; do
  codesign --verify --strict "$prefix/bin/$binary"
  details=$(codesign --display --verbose=4 "$prefix/bin/$binary" 2>&1)
  [[ $details == *'Authority=Developer ID Application:'* && $details == *'TeamIdentifier=658YBSK6MG'* ]]
done
test -f "$prefix/libexec/alondra/.claude-plugin/marketplace.json"
test -f "$prefix/libexec/alondra/plugin-orchestrator/skills/alondra-orchestrator/SKILL.md"
# Run the installed executable outside any checkout, without --repo/--bin.
# Its dependencies expose only read-only observations; setup must stay dry.
python3 deploy/brew/smoke-setup.py "$prefix"
