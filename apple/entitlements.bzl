load("@bazel_skylib//rules:write_file.bzl", "write_file")

_APP_IDENTIFIER_PREFIX = "$(AppIdentifierPrefix)"

_PLIST_HEAD = [
    "<?xml version=\"1.0\" encoding=\"UTF-8\"?>",
    "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">",
    "<plist version=\"1.0\">",
    "<dict>",
]

_PLIST_TAIL = ["</dict>", "</plist>", ""]

def _value_lines(value):
    if type(value) == "bool":
        return ["\t<true/>" if value else "\t<false/>"]
    if type(value) == "list":
        return ["\t<array>"] + ["\t\t<string>%s</string>" % item for item in value] + ["\t</array>"]
    return ["\t<string>%s</string>" % value]

def _plist(entries):
    lines = list(_PLIST_HEAD)
    for key, value in entries.items():
        lines.append("\t<key>%s</key>" % key)
        lines.extend(_value_lines(value))
    return "\n".join(lines + _PLIST_TAIL)

def _keychain_groups(*identifiers):
    return [_APP_IDENTIFIER_PREFIX + identifier for identifier in identifiers]

def _plist_file(name, out, entries, **kwargs):
    write_file(name = name, out = out, content = [_plist(entries)], **kwargs)

def _target_entitlements(name, target, bundle_id, push_group, team_id, aps_environments, **kwargs):
    groups = _keychain_groups(bundle_id, push_group)
    for variant in ["development", "app_store"]:
        entries = {}
        if variant == "app_store":
            entries["com.apple.developer.team-identifier"] = team_id
        if variant in aps_environments:
            entries["aps-environment"] = aps_environments[variant]
        entries["keychain-access-groups"] = groups
        _plist_file(
            name = "%s_%s_%s" % (name, target, variant),
            out = "%s_%s_%s.entitlements" % (name, target, variant),
            entries = entries,
            **kwargs
        )

def signed_entitlements(name, ios_bundle_id, notification_service_bundle_id, live_activity_bundle_id, team_id, **kwargs):
    """Writes the entitlements and the push Keychain group of every signed target.

    Every identifier comes from the signing file, so a build with another bundle id signs with
    entitlements that name it. The first Keychain group of each target is its own bundle id: items
    saved without an explicit group land there.

    Declares `<name>_<target>_<variant>` entitlements for the iOS app, the notification service
    and the Live Activity (`development` and `app_store`), `<name>_mac_app_store`, and
    `<name>_push_keychain_group`, an Info.plist fragment that names the group the app and its
    extensions share.
    """
    push_group = ios_bundle_id + ".push"
    _target_entitlements(name, "ios", ios_bundle_id, push_group, team_id, {"development": "development", "app_store": "production"}, **kwargs)
    _target_entitlements(name, "notification_service", notification_service_bundle_id, push_group, team_id, {}, **kwargs)
    _target_entitlements(name, "live_activity", live_activity_bundle_id, push_group, team_id, {}, **kwargs)
    _plist_file(
        name = name + "_mac_app_store",
        out = name + "_mac_app_store.entitlements",
        entries = {
            "com.apple.application-identifier": _APP_IDENTIFIER_PREFIX + ios_bundle_id,
            "com.apple.developer.team-identifier": team_id,
            "com.apple.security.app-sandbox": True,
            "com.apple.security.network.client": True,
            "com.apple.security.network.server": True,
            "com.apple.security.files.user-selected.read-write": True,
        },
        **kwargs
    )
    _plist_file(
        name = name + "_push_keychain_group",
        out = name + "_push_keychain_group.plist",
        entries = {"AlondraPushKeychainAccessGroup": "%s.%s" % (team_id, push_group)},
        **kwargs
    )

_ROLES = [
    "ios_development",
    "ios_app_store",
    "notification_service_development",
    "notification_service_app_store",
    "live_activity_development",
    "live_activity_app_store",
    "mac_app_store",
    "push_keychain_group",
]

def entitlements_follow_signing_test(name, entitlements, ios_bundle_id, notification_service_bundle_id, live_activity_bundle_id, team_id, forbidden = [], **kwargs):
    """Checks the files `signed_entitlements(entitlements)` wrote against the signing values.

    `forbidden` lists strings no generated file may contain.
    """
    labels = {role: ":%s_%s" % (entitlements, role) for role in _ROLES}
    native.sh_test(
        name = name,
        srcs = ["//apple:entitlements_follow_signing_test.py"],
        args = [
            "--ios-bundle-id=" + ios_bundle_id,
            "--notification-service-bundle-id=" + notification_service_bundle_id,
            "--live-activity-bundle-id=" + live_activity_bundle_id,
            "--team-id=" + team_id,
        ] + ["--forbidden=" + text for text in forbidden] +
        ["--%s=$(rootpath %s)" % (role.replace("_", "-"), label) for role, label in labels.items()],
        data = labels.values(),
        **kwargs
    )
