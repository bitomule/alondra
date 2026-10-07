#!/usr/bin/env python3
"""An App Store or TestFlight update keeps the app's Keychain items and container, so the new
build stays signed in and paired only if it looks for them where the old build wrote them:
the same Keychain service and account, the same default Keychain group and the same
Application Support folder. Changing any of these signs every phone out on update."""
import plistlib
import sys

DEFAULT_GROUP = "$(AppIdentifierPrefix)com.bitomule.alondra"

PINNED = {
    "KeychainRefreshTokenStorage.swift": [
        'service: String = "com.bitomule.alondra.auth"',
        'private let account = "oidc-refresh-token"',
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


def main(*paths):
    failures = []
    for path in paths:
        name = path.rsplit("/", 1)[-1]
        if name.endswith(".entitlements"):
            with open(path, "rb") as handle:
                groups = plistlib.load(handle).get("keychain-access-groups", [])
            # Items are saved without an explicit group, so they land in the first one.
            if not groups or groups[0] != DEFAULT_GROUP:
                failures.append(f"{path}: the first Keychain group must stay {DEFAULT_GROUP}, found {groups}")
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
