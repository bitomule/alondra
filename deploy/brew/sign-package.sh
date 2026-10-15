#!/usr/bin/env bash
# CI-only ephemeral keychain. Never enable shell tracing here.
set -euo pipefail
[[ $# == 3 ]] || { echo 'usage: sign-package.sh VERSION TARGET OUTPUT_DIR' >&2; exit 2; }
version=$1
target=$2
output=$3
for variable in DEVELOPER_ID_P12_BASE64 DEVELOPER_ID_P12_PASSWORD DEVELOPER_ID_IDENTITY APPLE_TEAM_ID ; do
  [[ -n ${!variable:-} ]] || { echo "Missing required secret: $variable" >&2; exit 1; }
done
[[ $APPLE_TEAM_ID == 658YBSK6MG ]] || { echo 'Unexpected signing team' >&2; exit 1; }
private=$(mktemp -d)
keychain="$private/release.keychain-db"
cleanup() {
  security delete-keychain "$keychain" >/dev/null 2>&1 || true
  rm -rf "$private"
}
trap cleanup EXIT
umask 077
printf '%s' "$DEVELOPER_ID_P12_BASE64" | base64 --decode > "$private/signing.p12"
keychain_password=$(openssl rand -hex 32)
security create-keychain -p "$keychain_password" "$keychain"
security set-keychain-settings -lut 21600 "$keychain"
security unlock-keychain -p "$keychain_password" "$keychain"
security import "$private/signing.p12" -k "$keychain" -P "$DEVELOPER_ID_P12_PASSWORD" -T /usr/bin/codesign >/dev/null
security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$keychain_password" "$keychain" >/dev/null
mkdir "$private/payload"
for binary in alondra alondrad alondrad-launcher; do
  identifier="com.bitomule.$binary"
  [[ $binary == alondrad-launcher ]] && identifier=com.bitomule.alondrad
  cp "target/$target/release/$binary" "$private/payload/$binary"
  codesign --force --options runtime --timestamp --keychain "$keychain" --sign "$DEVELOPER_ID_IDENTITY" --identifier "$identifier" "$private/payload/$binary"
  codesign --verify --strict "$private/payload/$binary"
  details=$(codesign --display --verbose=4 "$private/payload/$binary" 2>&1)
  [[ $details == *"TeamIdentifier=$APPLE_TEAM_ID"* && $details == *'Authority=Developer ID Application:'* && $details == *'Timestamp='* ]] || {
    echo 'Unexpected signing identity or missing timestamp' >&2; exit 1;
  }
done
bash deploy/brew/package-cli.sh "$version" "$target" "$private/payload/alondra" "$private/payload/alondrad" "$private/payload/alondrad-launcher" "$output"
python3 deploy/brew/release.py validate "$version" "$output/alondra-$version-$target.tar.gz"
