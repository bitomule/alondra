#!/usr/bin/env python3
"""An App Store or TestFlight update keeps the app's Keychain items and container, so the new
build stays signed in and paired only if it looks for them where the old build wrote them:
the same Keychain service and account, the same default Keychain group (the app's bundle id) and
the same Application Support folder. Changing any of these signs every phone out on update. The account is tied to the issuer, so
moving to another issuer signs out once, on purpose."""
import plistlib
import sys

APP_IDENTIFIER_PREFIX = "$(AppIdentifierPrefix)"

PINNED = {
    "KeychainRefreshTokenStorage.swift": [
        'service: String = "com.bitomule.alondra.auth"',
        'account = "oidc-refresh-token@\\(issuer.host() ?? "")"',
        'private static let previousIssuerAccount = "oidc-refresh-token"',
        "kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly",
    ],
    "PairingStore.swift": [
        'service: String = "com.bitomule.alondra.service-pairing.v1"',
        'kSecAttrAccount as String: "approved-phone"',
        "kSecAttrAccessibleWhenUnlockedThisDeviceOnly",
    ],
    "AccountBootstrap.swift": [
        'AppStorageLocation.applicationSupport(bundleID: "com.bitomule.alondra")',
    ],
    "AppStorageLocation.swift": [
        '.appendingPathComponent("CommandJournal", isDirectory: true)',
    ],
}


def main(*arguments):
    prefix = "--ios-bundle-id="
    bundle_id = next(argument for argument in arguments if argument.startswith(prefix))[len(prefix):]
    default_group = APP_IDENTIFIER_PREFIX + bundle_id
    paths = [argument for argument in arguments if not argument.startswith(prefix)]
    failures = []
    for path in paths:
        name = path.rsplit("/", 1)[-1]
        if name.endswith(".entitlements"):
            with open(path, "rb") as handle:
                groups = plistlib.load(handle).get("keychain-access-groups", [])
            # Items are saved without an explicit group, so they land in the first one.
            if not groups or groups[0] != default_group:
                failures.append(f"{path}: the first Keychain group must stay {default_group}, found {groups}")
            continue
        with open(path, encoding="utf-8") as handle:
            source = handle.read()
        for needle in PINNED.get(name, []):
            if needle not in source:
                failures.append(f"{path}: lost `{needle}`; the updated app would not find what the old build saved")
    missing = set(PINNED) - {path.rsplit("/", 1)[-1] for path in paths}
    failures += [f"{name} was not checked" for name in sorted(missing)]
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
