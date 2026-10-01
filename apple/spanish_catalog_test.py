#!/usr/bin/env python3
"""Every literal a SwiftUI view looks up in an app's Localizable catalog needs a Spanish translation."""
import json
import re
import sys

BRAND = {"ALONDRA"}
LITERAL = re.compile(
    r'(?:Text|Button|Label|Section|navigationTitle|scheduleSection|Toggle|Picker|TextField|Menu|ProgressView|Link'
    r'|help|accessibilityLabel|accessibilityHint|ContentUnavailableView|Stepper|DatePicker)\(\s*"([^"\\]+)"'
    r'|String\(localized:\s*"([^"\\]+)"'
)


def strings(path):
    return json.load(open(path))["strings"]


def translated(catalog, key):
    return "es" in catalog.get(key, {}).get("localizations", {})


def main():
    shared_path, ios_path, mac_path, *sources = sys.argv[1:]
    shared = strings(shared_path)
    catalogs = {"ios": strings(ios_path), "mac": strings(mac_path)}
    missing = []
    for source in sources:
        client = "mac" if "/macOS/" in source else "ios" if "/iOS/" in source else None
        for number, line in enumerate(open(source), 1):
            for match in LITERAL.finditer(line):
                key = match.group(1) or match.group(2)
                if key in BRAND or "\\(" in key or not re.search("[A-Za-z]", key):
                    continue
                if "AlondraDesign" in line and translated(shared, key):
                    continue
                for name, catalog in catalogs.items():
                    if client in (None, name) and not translated(catalog, key):
                        missing.append(f"{source}:{number}: {key!r} has no Spanish in the {name} catalog")
    if missing:
        print("\n".join(sorted(set(missing))), file=sys.stderr)
        sys.exit(1)


main()
