#!/usr/bin/env python3
"""Prepare and distribute sequential iOS and Mac TestFlight builds.

The default invocation is a local plan. ``--publish`` enables ASC writes and
local signing/builds. This adapter never edits, commits, tags, or pushes source.
"""
from __future__ import annotations

import argparse
import base64
import datetime as dt
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time

ROOT = Path(__file__).resolve().parents[2]
APPS = {
    "ios": {"id": "6816262826", "platform": "IOS", "suffix": ".ipa"},
    "macos": {"id": "6816263014", "platform": "MAC_OS", "suffix": ".pkg"},
}
LOCALES = ("en-US", "de-DE", "es-MX")


class ReleaseError(RuntimeError):
    pass


def run(argv: list[str], *, cwd: Path = ROOT, capture: bool = True) -> bytes:
    try:
        result = subprocess.run(argv, cwd=cwd, stdout=subprocess.PIPE if capture else None,
                                stderr=subprocess.PIPE, check=False)
    except OSError as error:
        raise ReleaseError(f"Required local command unavailable: {Path(argv[0]).name}") from error
    if result.returncode:
        # Command arguments may include notes or local paths; never echo them.
        raise ReleaseError(f"{Path(argv[0]).name} failed (exit {result.returncode})")
    return result.stdout or b""


def version_from_build_file(path: Path = ROOT / "apple/BUILD.bazel") -> tuple[str, str]:
    source = path.read_text()
    match = re.search(
        r'apple_bundle_version\(\s*name\s*=\s*"Version"\s*,\s*'
        r'build_version\s*=\s*"([1-9][0-9]*)"\s*,\s*'
        r'short_version_string\s*=\s*"([0-9]+\.[0-9]+\.[0-9]+)"', source)
    if not match:
        raise ReleaseError("Could not read Alondra's Version bundle settings")
    return match.group(2), match.group(1)


def read_notes(path: Path) -> dict[str, str]:
    try:
        source = path.read_text()
    except OSError as error:
        raise ReleaseError("Release-notes file could not be read") from error
    result: dict[str, str] = {}
    locale = None
    lines: list[str] = []
    for line in source.splitlines() + ["[END]"]:
        heading = re.fullmatch(r"\[([A-Za-z0-9-]+)\]", line.strip())
        if heading:
            if locale:
                result[locale] = "\n".join(lines).strip()
            locale = heading.group(1)
            if locale in result:
                raise ReleaseError(f"Duplicate release-notes locale: {locale}")
            lines = []
        elif locale and line.strip():
            lines.append(line.rstrip())
    missing = [item for item in LOCALES if not result.get(item)]
    extra = [item for item in result if item not in LOCALES]
    if missing or extra:
        raise ReleaseError("Notes must contain non-empty en-US, de-DE, and es-MX sections only")
    if any(len(body) > 4000 for body in result.values()):
        raise ReleaseError("A localized What to Test section exceeds Apple's 4,000-character limit")
    for body in result.values():
        bullets = [line for line in body.splitlines() if line.strip()]
        if not bullets or len(bullets) > 5 or any(not line.startswith("- ") for line in bullets):
            raise ReleaseError("Each locale must contain one to five '- ' What to Test bullets")
    return result


def asc_prefix(home: Path | None = None) -> list[str]:
    config = (home or Path.home()) / ".asc/config.json"
    try:
        value = json.loads(config.read_text()).get("default_key_name")
    except (OSError, ValueError, AttributeError):
        value = None
    return ["asc", "--profile", value] if isinstance(value, str) and value else ["asc"]


def json_data(raw: bytes, label: str) -> dict:
    try:
        value = json.loads(raw)
    except (ValueError, TypeError) as error:
        raise ReleaseError(f"{label} returned invalid JSON") from error
    if not isinstance(value, dict):
        raise ReleaseError(f"{label} returned an unexpected response")
    return value


def next_build_number(raw: bytes) -> int:
    data = json_data(raw, "ASC build-number lookup")
    candidates: list[object] = []

    def visit(value):
        if isinstance(value, dict):
            for key, item in value.items():
                if key in ("nextBuildNumber", "next_build_number"):
                    candidates.append(item)
                visit(item)
        elif isinstance(value, list):
            for item in value:
                visit(item)

    visit(data)
    valid = [int(value) for value in candidates if str(value).isdigit() and int(value) > 0]
    if not valid or len(set(valid)) != 1:
        raise ReleaseError("ASC did not return one unambiguous next build number")
    if valid[0] > 9999:
        raise ReleaseError("ASC's next build number exceeds Alondra's supported 9999 maximum")
    return valid[0]


def certificate_expiry(raw: bytes, identity_sha1: str, *, now: dt.datetime | None = None) -> dt.datetime:
    response = json_data(raw, "ASC certificate preflight")
    rows = response.get("data")
    if not isinstance(rows, list):
        raise ReleaseError("ASC certificate response has no certificate list")
    identity = identity_sha1.replace(":", "").upper()
    if not re.fullmatch(r"[0-9A-F]{40}", identity):
        raise ReleaseError("Distribution identity must be an explicit SHA-1 fingerprint")
    matches = []
    for row in rows:
        attributes = row.get("attributes", {}) if isinstance(row, dict) else {}
        kind = attributes.get("certificateType")
        if kind not in {"DISTRIBUTION", "IOS_DISTRIBUTION"}:
            continue
        content = attributes.get("certificateContent")
        expiration = attributes.get("expirationDate")
        if not content or not expiration:
            continue
        try:
            der = base64.b64decode(content, validate=True)
            expiry = dt.datetime.fromisoformat(expiration.replace("Z", "+00:00"))
        except (ValueError, TypeError, AttributeError):
            continue
        if hashlib.sha1(der).hexdigest().upper() == identity:
            matches.append(expiry)
    if not matches:
        raise ReleaseError("The selected signing identity was not found in ASC distribution certificates")
    expiry = max(matches)
    now = now or dt.datetime.now(dt.timezone.utc)
    if expiry.tzinfo is None:
        expiry = expiry.replace(tzinfo=dt.timezone.utc)
    if expiry <= now + dt.timedelta(days=14):
        raise ReleaseError("The selected Apple Distribution certificate expires within 14 days")
    return expiry


def validate_account_status(raw: bytes) -> None:
    """Fail closed on explicit ASC account/agreement blockers."""
    status = json_data(raw, "ASC account preflight")
    blocking = {"blocked", "unhealthy", "failed", "error", "not_accepted", "pending", "expired"}

    def visit(value, parent=""):
        if isinstance(value, dict):
            for key, item in value.items():
                folded = re.sub(r"[^a-z]", "", key.lower())
                if folded in {"healthy", "ishealthy", "agreementsaccepted", "isagreementaccepted"} and item is False:
                    raise ReleaseError("ASC account health or agreement preflight reported a blocker")
                if folded in {"status", "agreementstatus", "licensestatus"} and isinstance(item, str):
                    if item.lower().replace("-", "_").replace(" ", "_") in blocking:
                        raise ReleaseError("ASC account health or agreement preflight reported a blocker")
                if folded in {"blockingissues", "blockingerrors"} and item:
                    raise ReleaseError("ASC account health or agreement preflight reported a blocker")
                visit(item, folded)
        elif isinstance(value, list):
            for item in value:
                visit(item, parent)

    visit(status)


def matching_build(rows: bytes, *, build: int, macos: bool) -> dict | None:
    response = json_data(rows, "ASC build lookup")
    data = response.get("data")
    if not isinstance(data, list):
        raise ReleaseError("ASC build lookup returned no build list")
    matches = []
    for row in data:
        if not isinstance(row, dict):
            continue
        attrs = row.get("attributes", {})
        if not isinstance(attrs, dict):
            continue
        # App Store Connect's builds.attributes.version is CFBundleVersion
        # (the build number); marketing version lives on preReleaseVersion.
        if str(attrs.get("version", attrs.get("buildNumber"))) != str(build):
            continue
        if bool(attrs.get("lsMinimumSystemVersion")) != macos:
            continue
        matches.append(row)
    if len(matches) > 1:
        raise ReleaseError("ASC returned multiple matching builds; refusing ambiguous distribution")
    return matches[0] if matches else None


def group_id(raw: bytes, name: str) -> str:
    response = json_data(raw, "ASC beta-group lookup")
    rows = response.get("data")
    if not isinstance(rows, list):
        raise ReleaseError("ASC beta-group lookup returned no group list")
    ids = []
    for row in rows:
        if not isinstance(row, dict):
            continue
        attributes = row.get("attributes", {})
        if (isinstance(attributes, dict) and attributes.get("name") == name
                and attributes.get("isInternalGroup") is not True):
            ids.append(row.get("id"))
    if len(ids) != 1 or not isinstance(ids[0], str):
        raise ReleaseError("Expected exactly one matching external TestFlight group")
    return ids[0]


def group_ids(raw: bytes) -> set[str]:
    response = json_data(raw, "ASC build-group verification")
    # Current `asc testflight groups list --build-id ... --output json` returns
    # a summarized response with a top-level `groups` array. Keep accepting
    # the older JSON:API `data` shape for callers and saved fixtures.
    if "groups" in response:
        rows = response.get("groups")
        if response.get("complete") is not True:
            raise ReleaseError("ASC build-group verification is incomplete")
        count = response.get("groupCount")
        if not isinstance(count, int) or isinstance(count, bool):
            raise ReleaseError("ASC build-group verification returned an invalid group count")
    else:
        rows = response.get("data")
        count = len(rows) if isinstance(rows, list) else None
    if not isinstance(rows, list):
        raise ReleaseError("ASC build-group verification returned no group list")
    if count != len(rows):
        raise ReleaseError("ASC build-group verification group count does not match its list")
    ids = []
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get("id"), str) or not row["id"]:
            raise ReleaseError("ASC build-group verification returned a malformed group")
        ids.append(row["id"])
    if len(set(ids)) != len(ids):
        raise ReleaseError("ASC build-group verification returned duplicate group ids")
    return set(ids)


def validate_notes_response(raw: bytes, expected: dict[str, str]) -> None:
    response = json_data(raw, "ASC What to Test verification")
    rows = response.get("data")
    if not isinstance(rows, list):
        raise ReleaseError("ASC What to Test lookup returned no localization list")
    observed = {}
    for row in rows:
        if not isinstance(row, dict):
            continue
        attributes = row.get("attributes", {})
        locale = attributes.get("locale")
        text = attributes.get("whatsNew")
        if isinstance(locale, str) and isinstance(text, str):
            observed[locale] = text
    if any(observed.get(locale) != text for locale, text in expected.items()):
        raise ReleaseError("ASC What to Test localizations do not match the requested notes")


def existing_note_locales(raw: bytes) -> set[str]:
    response = json_data(raw, "ASC What to Test localization lookup")
    rows = response.get("data")
    if not isinstance(rows, list):
        raise ReleaseError("ASC What to Test lookup returned no localization list")
    locales = set()
    for row in rows:
        if not isinstance(row, dict):
            continue
        attributes = row.get("attributes", {})
        if isinstance(attributes, dict) and isinstance(attributes.get("locale"), str):
            locales.add(attributes["locale"])
    return locales


def notes_operation(locale: str, existing_locales: set[str]) -> str:
    return "update" if locale in existing_locales else "create"


def upsert_notes(call_asc, build_id: str, notes: dict[str, str]) -> dict[str, str]:
    existing = existing_note_locales(call_asc(
        "builds", "test-notes", "list", "--build-id", build_id,
        "--paginate", "--output", "json"))
    operations = {}
    for locale, text in notes.items():
        operation = notes_operation(locale, existing)
        command = ["builds", "test-notes", operation, "--build-id", build_id,
                   "--locale", locale, "--whats-new", text]
        try:
            call_asc(*command)
        except ReleaseError as error:
            raise ReleaseError(f"ASC could not {operation} What to Test locale {locale}") from error
        operations[locale] = operation
    return operations


def build_state(row: dict) -> str:
    attributes = row.get("attributes", {})
    return str(attributes.get("processingState", "")).upper()


def manifest_ok(path: Path, *, destination: str, version: str, build: int, source: str) -> dict:
    try:
        manifest = json.loads((path / "manifest.json").read_text())
        artifact_name = manifest["artifact"]
        artifact = path / artifact_name
    except (OSError, ValueError, KeyError, TypeError) as error:
        raise ReleaseError(f"{destination} artifact manifest is missing or invalid") from error
    expected_name = "Alondra.ipa" if destination == "ios" else "Alondra.pkg"
    if (artifact_name != expected_name or manifest.get("destination") != destination or manifest.get("version") != version
            or str(manifest.get("build")) != str(build) or manifest.get("source") != source
            or not artifact.is_file() or sha256(artifact) != manifest.get("sha256")):
        raise ReleaseError(f"{destination} artifact did not pass manifest verification")
    return {"manifest": manifest, "artifact": artifact}


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(block)
    return digest.hexdigest()


class BetaRun:
    def __init__(self, args):
        self.args = args
        self.args.notes = args.notes.expanduser().resolve()
        self.args.output_root = args.output_root.expanduser().resolve()
        self.args.identity = args.identity.replace(":", "").upper()
        self.args.installer_identity = args.installer_identity.replace(":", "").upper()
        if not args.ios_group.strip() or not args.macos_group.strip():
            raise ReleaseError("Both app-specific external TestFlight group names are required")
        for identity in (self.args.identity, self.args.installer_identity):
            if not re.fullmatch(r"[0-9A-F]{40}", identity):
                raise ReleaseError("Signing identities must be explicit SHA-1 fingerprints")
        self.asc = asc_prefix()
        self.source = ""
        self.version, _ = version_from_build_file()
        self.notes = read_notes(args.notes)
        self.result = {"version": self.version, "platforms": {}}
        self.build_numbers: dict[str, int] = {}
        self.groups: dict[str, str] = {}

    def call_asc(self, *args: str) -> bytes:
        return run(self.asc + list(args))

    def preflight(self):
        if self.args.notes.resolve().is_relative_to(ROOT):
            raise ReleaseError("Keep the release-notes file outside the source checkout")
        branch = run(["git", "branch", "--show-current"]).decode().strip()
        if branch != "main":
            raise ReleaseError("Release requires the reviewed main branch")
        if run(["git", "status", "--porcelain", "--untracked-files=normal"]).strip():
            raise ReleaseError("Release checkout is dirty; use a clean isolated checkout")
        run(["git", "fetch", "origin", "main", "-q"])
        head = run(["git", "rev-parse", "HEAD"]).decode().strip()
        origin = run(["git", "rev-parse", "origin/main"]).decode().strip()
        if head != origin:
            raise ReleaseError("Checkout must equal origin/main exactly; do not release from an unreviewed commit")
        self.source = head
        self.call_asc("auth", "status", "--validate")
        for platform in APPS:
            app = APPS[platform]
            validate_account_status(self.call_asc("account", "status", "--app", app["id"], "--output", "json"))
        certs = self.call_asc("certificates", "list", "--fields", "certificateType,expirationDate,certificateContent", "--paginate", "--output", "json")
        certificate_expiry(certs, self.args.identity)
        for destination, group_name in (("ios", self.args.ios_group), ("macos", self.args.macos_group)):
            app = APPS[destination]
            self.build_numbers[destination] = next_build_number(
                self.call_asc("builds", "next-build-number", "--app", app["id"],
                              "--platform", app["platform"], "--output", "json"))
            self.groups[destination] = group_id(
                self.call_asc("testflight", "groups", "list", "--app", app["id"],
                              "--external", "--name", group_name, "--paginate", "--output", "json"), group_name)
        # The existing Alondra preparation gate checks the chosen local identities,
        # embedded provisioning profiles, ignored credentials and clean Git state.
        for destination in APPS:
            command = [sys.executable, str(ROOT / "deploy/apple/prepare.py"), destination,
                       "--version", self.version, "--build-number", "1",
                       "--identity", self.args.identity,
                       "--output", str(self.args.output_root / destination), "--preflight-only"]
            if destination == "macos":
                command += ["--installer-identity", self.args.installer_identity]
            self._bazel_storage(command)
            run(command)
        self.result["source"] = head

    def _bazel_storage(self, command: list[str]):
        if self.args.bazel_output_user_root:
            command.extend(["--bazel-output-user-root", self.args.bazel_output_user_root])
        if self.args.bazel_output_base:
            command.extend(["--bazel-output-base", self.args.bazel_output_base])

    def publish(self):
        if self.args.output_root.exists():
            raise ReleaseError("Artifact output root already exists; choose a new private directory")
        if self.args.output_root.resolve().is_relative_to(ROOT):
            raise ReleaseError("Artifact output root must be outside the checkout")
        self.preflight()
        self.args.output_root.mkdir(parents=True, mode=0o700)
        self.persist()
        for destination in ("ios", "macos"):
            try:
                self.release_platform(destination)
            except ReleaseError as error:
                status = self.result["platforms"].get(destination, {})
                if status.get("state") in {"uploading", "uploaded", "processing-verified", "notes-verified", "grouped"}:
                    status["state"] = "partial"
                    self.persist()
                    build = status.get("build_id", status.get("build", "unknown"))
                    platform = APPS[destination]["platform"]
                    raise ReleaseError(
                        f"{destination} build {build} may already be uploaded; do not rerun or upload it again. "
                        f"Inspect {self.args.output_root / 'release-result.json'} and ASC build status for app "
                        f"{APPS[destination]['id']} / {platform}. Repair notes, group, or review on that exact build. "
                        f"Earlier platform status is preserved in the result file. Last step: {error}"
                    ) from error
                raise
        self.result["complete"] = True
        self.persist()

    def release_platform(self, destination: str):
        app = APPS[destination]
        number = self.build_numbers[destination]
        self.result["platforms"][destination] = {"build": number, "state": "preparing"}
        self.persist()
        group = self.groups[destination]
        output = self.args.output_root / destination
        command = [sys.executable, str(ROOT / "deploy/apple/prepare.py"), destination,
                   "--version", self.version, "--build-number", str(number),
                   "--identity", self.args.identity, "--output", str(output)]
        if destination == "macos":
            command += ["--installer-identity", self.args.installer_identity]
        self._bazel_storage(command)
        run(command, capture=False)
        artifact_data = manifest_ok(output, destination=destination, version=self.version,
                                    build=number, source=self.source)
        self.result["platforms"][destination].update(state="built", sha256=artifact_data["manifest"]["sha256"])
        self.persist()

        # Recheck immediately before upload so a concurrent upload cannot make this
        # artifact stale or cause a duplicate build number.
        current_next = next_build_number(self.call_asc("builds", "next-build-number", "--app", app["id"],
                                                        "--platform", app["platform"], "--output", "json"))
        existing = self.call_asc("builds", "list", "--app", app["id"],
                                 "--build-number", str(number), "--platform", app["platform"],
                                 "--paginate", "--output", "json")
        if current_next != number or matching_build(existing, build=number, macos=destination == "macos"):
            raise ReleaseError(f"{destination} build number changed or already exists; preserve artifacts and inspect ASC before retrying")

        self.result["platforms"][destination]["state"] = "uploading"
        self.persist()
        upload = ["builds", "upload", "--app", app["id"], "--version", self.version,
                  "--build-number", str(number), "--wait", "--output", "json"]
        upload += ["--ipa", str(artifact_data["artifact"])] if destination == "ios" else ["--pkg", str(artifact_data["artifact"])]
        self.call_asc(*upload)
        self.result["platforms"][destination]["state"] = "uploaded"
        self.persist()
        row = self.wait_for_valid_build(app["id"], destination, number)
        build_id = row.get("id")
        if not isinstance(build_id, str) or not build_id:
            raise ReleaseError(f"{destination} processed build has no ASC identifier; do not re-upload")
        self.result["platforms"][destination].update(state="processing-verified", build_id=build_id)
        self.persist()

        upsert_notes(self.call_asc, build_id, self.notes)
        validate_notes_response(self.call_asc("builds", "test-notes", "list", "--build-id", build_id,
                                              "--paginate", "--output", "json"), self.notes)
        self.result["platforms"][destination]["state"] = "notes-verified"
        self.persist()
        self.call_asc("builds", "add-groups", "--build-id", build_id,
                      "--group", group, "--submit", "--confirm")
        groups = self.call_asc("testflight", "groups", "list", "--build-id", build_id, "--app", app["id"], "--output", "json")
        if group not in group_ids(groups):
            raise ReleaseError(f"{destination} build is processed but its TestFlight group was not confirmed; do not re-upload")
        self.result["platforms"][destination].update(state="grouped", group_id=group)
        self.persist()
        self.call_asc("testflight", "notifications", "send", "--build-id", build_id)
        self.result["platforms"][destination].update(state="distributed", group_id=group)
        self.persist()

    def wait_for_valid_build(self, app_id: str, destination: str, number: int) -> dict:
        app = APPS[destination]
        for attempt in range(20):
            raw = self.call_asc("builds", "list", "--app", app_id,
                                "--build-number", str(number), "--platform", app["platform"],
                                "--paginate", "--output", "json")
            row = matching_build(raw, build=number, macos=destination == "macos")
            if row:
                state = build_state(row)
                if state in {"FAILED", "INVALID"}:
                    raise ReleaseError(f"{destination} build processing failed; do not re-upload")
                if state == "VALID":
                    return row
            if attempt < 19:
                time.sleep(30)
        raise ReleaseError(f"{destination} upload exists but processing was not verified after 10 minutes; do not re-upload")

    def persist(self):
        if not self.args.output_root.exists():
            return
        destination = self.args.output_root / "release-result.json"
        temporary = destination.with_suffix(".tmp")
        temporary.write_text(json.dumps(self.result, indent=2) + "\n")
        os.chmod(temporary, 0o600)
        temporary.replace(destination)


def parser():
    value = argparse.ArgumentParser(description=__doc__)
    value.add_argument("--notes", required=True, type=Path, help="Localized [en-US], [de-DE], [es-MX] What to Test file")
    value.add_argument("--ios-group", required=True, help="Exact external TestFlight group name for the iOS app")
    value.add_argument("--macos-group", required=True, help="Exact external TestFlight group name for the Mac app")
    value.add_argument("--identity", required=True, help="Existing Apple Distribution SHA-1 fingerprint")
    value.add_argument("--installer-identity", required=True, help="Existing Mac Installer SHA-1 fingerprint")
    value.add_argument("--output-root", required=True, type=Path, help="New private artifact directory outside checkout")
    value.add_argument("--bazel-output-user-root")
    value.add_argument("--bazel-output-base")
    value.add_argument("--publish", action="store_true", help="Run ASC preflight, signed builds, uploads and TestFlight distribution")
    return value


def main(argv=None) -> int:
    args = parser().parse_args(argv)
    try:
        release = BetaRun(args)
        if not args.publish:
            print(json.dumps({"mode": "plan-only", "source_checkout": str(ROOT),
                              "version": release.version, "apps": APPS,
                              "groups": {"ios": args.ios_group, "macos": args.macos_group},
                              "localization": list(release.notes),
                              "writes": "none; pass --publish to enable signing and ASC operations"}, indent=2))
            return 0
        release.publish()
        print("Both Alondra TestFlight builds processed and distributed successfully.")
        return 0
    except (ReleaseError, OSError) as error:
        print(f"Apple beta blocked: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
