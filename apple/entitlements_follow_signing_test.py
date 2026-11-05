#!/usr/bin/env python3
"""The entitlements and the push Keychain group of every signed target are written from the
signing file's identifiers: the bundle id of each target is its first Keychain group, all of them
share the push group, and nothing the signing file does not name appears in them."""
import argparse
import plistlib
import sys

APP_IDENTIFIER_PREFIX = "$(AppIdentifierPrefix)"
ROLES = [
    "ios_development",
    "ios_app_store",
    "notification_service_development",
    "notification_service_app_store",
    "live_activity_development",
    "live_activity_app_store",
    "mac_app_store",
    "push_keychain_group",
]


def main(*arguments):
    parser = argparse.ArgumentParser()
    for name in ["ios-bundle-id", "notification-service-bundle-id", "live-activity-bundle-id", "team-id"]:
        parser.add_argument("--" + name, required=True)
    parser.add_argument("--forbidden", action="append", default=[])
    for role in ROLES:
        parser.add_argument("--" + role.replace("_", "-"), required=True)
    options = vars(parser.parse_args(arguments))

    bundle_ids = {
        "ios": options["ios_bundle_id"],
        "notification_service": options["notification_service_bundle_id"],
        "live_activity": options["live_activity_bundle_id"],
    }
    team = options["team_id"]
    push_group = bundle_ids["ios"] + ".push"
    app_group = "group." + bundle_ids["ios"]
    failures = []

    def expect(path, key, expected, values):
        if values.get(key) != expected:
            failures.append(f"{path}: {key} is {values.get(key)!r}, expected {expected!r}")

    for role in ROLES:
        path = options[role]
        with open(path, "rb") as handle:
            raw = handle.read()
        for text in options["forbidden"]:
            if text.encode() in raw:
                failures.append(f"{path} contains {text}")
        values = plistlib.loads(raw)
        if role == "push_keychain_group":
            expect(path, "AlondraPushKeychainAccessGroup", f"{team}.{push_group}", values)
            expect(path, "AlondraActivityAppGroup", app_group, values)
        elif role == "mac_app_store":
            expect(path, "com.apple.application-identifier", APP_IDENTIFIER_PREFIX + bundle_ids["ios"], values)
            expect(path, "com.apple.developer.team-identifier", team, values)
            expect(path, "com.apple.security.app-sandbox", True, values)
        else:
            target = role.removesuffix("_development").removesuffix("_app_store")
            expect(path, "keychain-access-groups", [APP_IDENTIFIER_PREFIX + bundle_ids[target], APP_IDENTIFIER_PREFIX + push_group], values)
            app_store = role.endswith("_app_store")
            expect(path, "com.apple.developer.team-identifier", team if app_store else None, values)
            push_environment = ("production" if app_store else "development") if target == "ios" else None
            expect(path, "aps-environment", push_environment, values)
            shares_files = target in ("ios", "live_activity")
            expect(path, "com.apple.security.application-groups", [app_group] if shares_files else None, values)
    for failure in failures:
        print(failure, file=sys.stderr)
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main(*sys.argv[1:]))
