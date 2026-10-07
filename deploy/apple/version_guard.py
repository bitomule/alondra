#!/usr/bin/env python3
"""Refuse a release whose version would go below what App Store Connect already has.

TestFlight never offers a build whose marketing version is lower than the one a tester has
installed, and an equal version needs a higher build number. iOS and Mac share one App Store
Connect record and tools/shared.bzl, so the check covers the builds of both platforms.
"""
import json
from pathlib import Path
import subprocess
import sys

sys.path.insert(0, str(Path(__file__).resolve().parent))
from prepare import GateError, read_versions  # noqa: E402

APP_ID = "6816262826"


class GuardError(Exception):
    pass


def parse_version(text):
    return tuple(int(part) for part in text.split("."))


def uploaded(raw):
    """(marketing version, build number) of every build in an `asc builds list` response."""
    try:
        document, _ = json.JSONDecoder().raw_decode(raw.lstrip())
    except ValueError as error:
        raise GuardError("App Store Connect answered with something that is not JSON") from error
    versions = {item["id"]: item["attributes"]["version"] for item in document.get("included", [])
                if item.get("type") == "preReleaseVersions"}
    result = []
    for build in document.get("data", []):
        reference = ((build.get("relationships") or {}).get("preReleaseVersion") or {}).get("data") or {}
        version = versions.get(reference.get("id"))
        number = str(build.get("attributes", {}).get("version", ""))
        if version and number.isdigit():
            result.append((parse_version(version), int(number)))
    return result


def check(local_version, local_build, builds):
    """None when the release can go out, otherwise why not."""
    if not builds:
        return None
    highest = max(version for version, _ in builds)
    if local_version < highest:
        return "version {} is lower than {} already in App Store Connect".format(
            ".".join(map(str, local_version)), ".".join(map(str, highest)))
    if local_version == highest:
        top_build = max(number for version, number in builds if version == highest)
        if local_build <= top_build:
            return "build {} of {} is not above build {} already in App Store Connect".format(
                local_build, ".".join(map(str, local_version)), top_build)
    return None


def fetch(app_id):
    result = subprocess.run(["asc", "builds", "list", "--app", app_id, "--include", "preReleaseVersion",
                             "--paginate", "--output", "json"], capture_output=True, text=True)
    if result.returncode:
        raise GuardError("asc builds list --app {} failed (exit {})".format(app_id, result.returncode))
    return uploaded(result.stdout)


def main():
    version, build = read_versions()
    problem = check(parse_version(version), int(build), fetch(APP_ID))
    if problem:
        raise GuardError("tools/shared.bzl is behind App Store Connect: " + problem
                         + ".\nRaise version (or build_number) in tools/shared.bzl.")
    print("Version {} ({}) is above every build in App Store Connect".format(version, build))


if __name__ == "__main__":
    try:
        main()
    except (GuardError, GateError) as error:
        print("Apple version guard: " + str(error), file=sys.stderr)
        sys.exit(1)
