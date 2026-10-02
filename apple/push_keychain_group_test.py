#!/usr/bin/env python3
"""The app writes push decryption keys to the Keychain group its Info.plist names and the
notification service extension reads the group its own Info.plist names. Both must be the
same group, and every entitlements variant of both targets must grant it."""
import plistlib
import sys

APP_IDENTIFIER_PREFIX = "$(AppIdentifierPrefix)"


def load(path):
    with open(path, "rb") as handle:
        return plistlib.load(handle)


def main(app_plist, extension_plist, *entitlements):
    groups = {path: load(path).get("AlondraPushKeychainAccessGroup") for path in (app_plist, extension_plist)}
    failures = [f"{path} names no AlondraPushKeychainAccessGroup" for path, group in groups.items() if not group]
    if len(set(groups.values())) != 1:
        failures.append(f"app and extension read different push Keychain groups: {groups}")
    group = groups[app_plist] or ""
    team, _, suffix = group.partition(".")
    for path in entitlements:
        values = load(path)
        granted = values.get("keychain-access-groups", [])
        if APP_IDENTIFIER_PREFIX + suffix not in granted and group not in granted:
            failures.append(f"{path} does not grant {group}: {granted}")
        declared_team = values.get("com.apple.developer.team-identifier")
        if declared_team and declared_team != team:
            failures.append(f"{path} is for team {declared_team}, the push group for {team}")
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
