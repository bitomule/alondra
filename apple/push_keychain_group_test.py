#!/usr/bin/env python3
"""The app writes push decryption keys to the Keychain group the generated Info.plist fragment
names, and the notification service and Live Activity extensions read the same one. No static
Info.plist may name a group of its own, and every entitlements variant of every target must grant
the generated group.

Arguments: the generated Info.plist fragment, `--static-plists` and the static Info.plists,
`--entitlements` and the entitlements."""
import plistlib
import sys

APP_IDENTIFIER_PREFIX = "$(AppIdentifierPrefix)"
KEY = "AlondraPushKeychainAccessGroup"


def load(path):
    with open(path, "rb") as handle:
        return plistlib.load(handle)


def main(*arguments):
    static_at = arguments.index("--static-plists")
    entitlements_at = arguments.index("--entitlements")
    generated = arguments[0]
    static_plists = arguments[static_at + 1:entitlements_at]
    entitlements = arguments[entitlements_at + 1:]
    group = load(generated).get(KEY)
    failures = [] if group else [f"{generated} names no {KEY}"]
    failures += [f"{path} names a {KEY} of its own" for path in static_plists if KEY in load(path)]
    team, _, suffix = (group or "").partition(".")
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
