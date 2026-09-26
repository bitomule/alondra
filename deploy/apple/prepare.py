#!/usr/bin/env python3
"""Build and verify local Apple distribution artifacts; never upload or modify ASC."""
import argparse
import base64
import datetime as dt
import hashlib
import json
import os
from pathlib import Path, PurePosixPath
import plistlib
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
import xml.etree.ElementTree as ET

ROOT = Path(__file__).resolve().parents[2]
TEAM = "658YBSK6MG"
IOS = "com.bitomule.alondra"
MAC = IOS + ".mac"
EXT = IOS + ".NotificationService"
PROFILES = {
    "ios": [(IOS, "Alondra_AppStore.mobileprovision"), (EXT, "Alondra_NotificationService_AppStore.mobileprovision")],
    "macos": [(MAC, "Alondra_Mac_AppStore.provisionprofile")],
}


class GateError(Exception):
    pass


def require(value, message):
    if not value:
        raise GateError(message)


def run(*args):
    # No shell, credential arguments or raw error bodies in output.
    result = subprocess.run([str(a) for a in args], capture_output=True, env={**os.environ, "LC_ALL": "C"})
    if result.returncode:
        directory = Path(tempfile.mkdtemp(prefix="alondra-apple-validation-"))
        log = directory / "failure.log"
        # security cms stdout can contain a complete profile; never log it.
        text = result.stderr.decode(errors="replace")
        if Path(str(args[0])).name in {"bazelisk", "codesign", "productbuild", "pkgutil", "xcrun"}:
            text += result.stdout.decode(errors="replace")
        text = re.sub(r"(?im)^.*(?:authorization|bearer |password|token|secret|api.?key|remote_header).*$", "[redacted]", text)
        with log.open("x") as stream:
            os.chmod(log, 0o600)
            stream.write(f"{Path(str(args[0])).name}: exit {result.returncode}\n" + text[-65536:])
        raise GateError(f"{args[0]} failed (exit {result.returncode}); artifact not approved. Private diagnostic: {log}")
    return result.stdout


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def read_plist(path):
    with path.open("rb") as stream:
        return plistlib.load(stream)


def validate_profile(profile, bundle_id, destination, identity, now=None):
    now = now or dt.datetime.now(dt.timezone.utc)
    expiry = profile.get("ExpirationDate")
    require(isinstance(expiry, dt.datetime), "Profile has no expiration")
    if expiry.tzinfo is None:
        expiry = expiry.replace(tzinfo=dt.timezone.utc)
    require(expiry > now, "Expired profile")
    require(profile.get("TeamIdentifier") == [TEAM], "Profile team mismatch")
    require(profile.get("ApplicationIdentifierPrefix") == [TEAM], "Profile app identifier prefix mismatch")
    require(not profile.get("ProvisionedDevices") and not profile.get("ProvisionsAllDevices"), "Development/ad hoc/enterprise profile refused")
    ent = profile.get("Entitlements", {})
    require(not ent.get("get-task-allow") and not ent.get("com.apple.security.get-task-allow"), "Debugging entitlement refused")
    app_id = ent.get("application-identifier", ent.get("com.apple.application-identifier"))
    require(app_id == TEAM + "." + bundle_id, "Profile bundle identifier mismatch")
    require(any(hashlib.sha1(cert).hexdigest().upper() == identity for cert in profile.get("DeveloperCertificates", [])), "Signing identity absent from profile")
    require(("iOS" if destination == "ios" else "OSX") in profile.get("Platform", []), "Profile platform mismatch")
    if bundle_id == IOS:
        require(ent.get("aps-environment") == "production", "APNs distribution profile must use production")
    return ent


def local_identity(fingerprint, installer=False):
    require(re.fullmatch(r"[0-9A-F]{40}", fingerprint), "Identity must be an explicit SHA-1 fingerprint")
    output = run("security", "find-identity", "-v", "-p", "basic" if installer else "codesigning").decode()
    matches = re.findall(r'([0-9A-F]{40}) "([^"]+)"', output)
    names = [name for value, name in matches if value == fingerprint]
    require(len(names) == 1, "Requested valid signing identity unavailable")
    name = names[0]
    required = "3rd Party Mac Developer Installer:" if installer else "Apple Distribution:"
    require(name.startswith(required) and name.endswith("(" + TEAM + ")"), "Wrong distribution identity type/team")
    # The installed productbuild accepts the exact SHA-1 selector, verified with
    # a real signed empty package while an expired same-name identity coexists.
    # Still verify the resulting package leaf: selector support is not trust proof.
    return fingerprint if installer else name


def verify_pkg_trust(output):
    status = re.search(r"^\s*Status:\s*(.+)$", output.decode(errors="replace"), re.MULTILINE)
    require(status and status.group(1).startswith("signed by a developer certificate issued by Apple"),
            "Installer package trust not established; pkgutil exit status alone is insufficient")


def verify_pkg_leaf(toc, fingerprint):
    root = ET.fromstring(toc)
    def children(nodes, name):
        return [child for node in nodes for child in node if child.tag.split("}")[-1] == name]
    signatures = children(children([root], "toc"), "signature")
    require(len(signatures) == 1, "Expected exactly one package signature")
    certificates = children(children(children(signatures, "KeyInfo"), "X509Data"), "X509Certificate")
    require(certificates and certificates[0].text, "Missing Installer leaf certificate")
    leaf = base64.b64decode("".join(certificates[0].text.split()), validate=True)
    require(hashlib.sha1(leaf).hexdigest().upper() == fingerprint, "Actual Installer leaf differs from requested identity")


def snapshot_archive(source, directory, destination):
    # Bazel outputs may be replaced by another invocation. All subsequent reads,
    # validation and packaging must use this independent private copy.
    snapshot = directory / ("input.ipa" if destination == "ios" else "input.zip")
    with source.open("rb") as incoming, snapshot.open("xb") as outgoing:
        os.chmod(snapshot, 0o600)
        shutil.copyfileobj(incoming, outgoing)
        outgoing.flush()
        os.fsync(outgoing.fileno())
    os.chmod(snapshot, 0o400)
    return snapshot, validate_archive(snapshot, destination)


def validate_archive(path, destination):
    """Reject wrong-platform bundles and unsafe ZIPs before extraction/signing tools."""
    with zipfile.ZipFile(path) as archive:
        names = archive.namelist()
        require(len(names) == len(set(names)), "Duplicate ZIP entries")
        for info in archive.infolist():
            parts = PurePosixPath(info.filename).parts
            require(parts and not info.filename.startswith("/") and ".." not in parts and "\\" not in info.filename, "Unsafe ZIP path")
            if stat.S_ISLNK(info.external_attr >> 16):
                target = archive.read(info).decode()
                resolved = os.path.normpath(str(PurePosixPath(info.filename).parent / target))
                require(not target.startswith("/") and ".." not in PurePosixPath(target).parts and not resolved.startswith("../") and resolved != "..", "Unsafe ZIP symlink")
        app = "Payload/Alondra.app" if destination == "ios" else "Alondra.app"
        require(all(name.startswith(app + "/") or name.rstrip("/") in {app, "Payload"} for name in names), "Unexpected archive payload")
        plist_path = app + ("/Info.plist" if destination == "ios" else "/Contents/Info.plist")
        info = plistlib.loads(archive.read(plist_path))
        require(info.get("DTPlatformName") == ("iphoneos" if destination == "ios" else "macosx"), "Simulator or wrong-platform archive refused")
        require(info.get("CFBundleSupportedPlatforms") == (["iPhoneOS"] if destination == "ios" else ["MacOSX"]), "Unsupported bundle platform")
        extensions = sorted({name.split(".appex/")[0] + ".appex" for name in names if ".appex/" in name})
        expected = [app + "/PlugIns/AlondraNotificationService.appex"] if destination == "ios" else []
        require(extensions == expected, "Missing or unexpected app extension")
        return app


def verify_bundle(bundle, bundle_id, destination, version, build, identity, profile_path):
    content = bundle if destination == "ios" else bundle / "Contents"
    info = read_plist(content / "Info.plist")
    require(info.get("CFBundleIdentifier") == bundle_id, "Built bundle ID mismatch")
    require(info.get("CFBundleShortVersionString") == version and info.get("CFBundleVersion") == build, "Built version mismatch")
    require(info.get("DTPlatformName") == ("iphoneos" if destination == "ios" else "macosx"), "Built platform mismatch")
    executable = info.get("CFBundleExecutable", "")
    require(executable and Path(executable).name == executable, "Invalid executable name")
    binary = content / executable if destination == "ios" else content / "MacOS" / executable
    platform = run("xcrun", "vtool", "-show-build", binary).decode()
    platforms = re.findall(r"^\s*platform\s+(\S+)", platform, re.MULTILINE)
    require(platforms and set(platforms) == {"IOS" if destination == "ios" else "MACOS"}, "Mach-O is simulator/wrong platform")
    run("codesign", "--verify", "--deep", "--strict", bundle)
    ent = plistlib.loads(run("codesign", "-d", "--entitlements", ":-", bundle))
    require(not ent.get("get-task-allow") and not ent.get("com.apple.security.get-task-allow"), "Signed app permits debugging")
    require(ent.get("com.apple.developer.team-identifier") == TEAM, "Signed team mismatch")
    require(ent.get("application-identifier", ent.get("com.apple.application-identifier")) == TEAM + "." + bundle_id, "Signed application identifier mismatch")
    if destination == "ios":
        require(set(ent.get("keychain-access-groups", [])) == {TEAM + "." + bundle_id, TEAM + "." + IOS + ".push"}, "Signed shared Keychain groups mismatch")
        if bundle_id == IOS:
            require(ent.get("aps-environment") == "production", "Signed APNs environment must be production")
    else:
        for key in ("com.apple.security.app-sandbox", "com.apple.security.network.client", "com.apple.security.files.user-selected.read-write"):
            require(ent.get(key) is True, "Missing Mac sandbox capability: " + key)
    embedded = content / ("embedded.mobileprovision" if destination == "ios" else "embedded.provisionprofile")
    require(embedded.is_file() and digest(embedded) == digest(profile_path), "Embedded profile differs from validated input")
    with tempfile.TemporaryDirectory(prefix="alondra-signer-") as directory:
        prefix = str(Path(directory) / "certificate")
        run("codesign", "-d", "--extract-certificates=" + prefix, bundle)
        require(hashlib.sha1(Path(prefix + "0").read_bytes()).hexdigest().upper() == identity, "Actual signer differs from requested identity")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("destination", choices=PROFILES)
    parser.add_argument("--version", required=True, help="Explicit x.y.z marketing version")
    parser.add_argument("--build-number", required=True)
    parser.add_argument("--identity", required=True, help="Existing Apple Distribution SHA-1")
    parser.add_argument("--installer-identity", help="Existing Mac Installer SHA-1 (macos only)")
    parser.add_argument("--output", required=True, type=Path, help="New private directory outside checkout")
    parser.add_argument("--bazel-output-user-root", type=Path, help="Reuse the coordinated local Bazel storage root")
    parser.add_argument("--bazel-output-base", type=Path, help="Reuse the coordinated local Bazel output base")
    parser.add_argument("--preflight-only", action="store_true", help="Validate existing local signing inputs without building")
    args = parser.parse_args()
    os.chdir(ROOT)
    require(re.fullmatch(r"[0-9]+\.[0-9]+\.[0-9]+", args.version), "Version must be x.y.z")
    require(re.fullmatch(r"[1-9][0-9]{0,3}", args.build_number), "Build number must be an integer from 1 through 9999")
    args.identity = args.identity.upper()
    local_identity(args.identity)
    installer = None
    if args.destination == "macos":
        require(args.installer_identity, "Mac distribution needs an Installer identity")
        installer = local_identity(args.installer_identity.upper(), installer=True)
    profiles = {}
    profile_hashes = {}
    for bundle_id, filename in PROFILES[args.destination]:
        path = ROOT / "signing" / filename
        require(path.is_file(), "Missing ignored profile: signing/" + filename)
        profile = plistlib.loads(run("security", "cms", "-D", "-i", path))
        validate_profile(profile, bundle_id, args.destination, args.identity)
        profiles[bundle_id] = path
        profile_hashes[bundle_id] = digest(path)
    require(not run("git", "-C", ROOT, "status", "--porcelain", "--untracked-files=normal").strip(), "Use a clean committed checkout")
    source_commit = run("git", "-C", ROOT, "rev-parse", "HEAD").decode().strip()
    if args.preflight_only:
        print("Local profile/identity preflight passed; no build or publication performed.")
        return
    output = args.output.resolve()
    require(not output.exists() and not output.is_relative_to(ROOT), "Output must be a new directory outside checkout")
    output.mkdir(parents=True, mode=0o700)
    target = "//apple:AlondraiOS" if args.destination == "ios" else "//apple:AlondraMac"
    flags = ["--define=alondra_distribution=1", "--embed_label=alondra_" + args.version + "_" + args.build_number,
             "--@build_bazel_rules_apple//apple/build_settings:signing_certificate_name=" + args.identity,
             "--ios_multi_cpus=arm64" if args.destination == "ios" else "--macos_cpus=arm64",
             "--jobs=1", "-c", "opt"]
    print("Building local " + args.destination + " distribution artifact…", flush=True)
    bazel = ["bazelisk"]
    if args.bazel_output_user_root:
        bazel.append("--output_user_root=" + str(args.bazel_output_user_root.resolve()))
    if args.bazel_output_base:
        bazel.append("--output_base=" + str(args.bazel_output_base.resolve()))
    run(*bazel, "build", target, *flags)
    artifacts = run(*bazel, "cquery", target, *flags, "--output=files").decode().splitlines()
    suffix = ".ipa" if args.destination == "ios" else ".zip"
    archives = [ROOT / p for p in artifacts if p.endswith(suffix)]
    require(len(archives) == 1, "Expected one Bazel archive")
    source = archives[0]
    require(all(digest(path) == profile_hashes[key] for key, path in profiles.items()), "Profile changed during build")
    require(run("git", "-C", ROOT, "rev-parse", "HEAD").decode().strip() == source_commit
            and not run("git", "-C", ROOT, "status", "--porcelain", "--untracked-files=normal").strip(), "Source changed during build")
    with tempfile.TemporaryDirectory(prefix="alondra-package-", dir=output) as directory:
        private = Path(directory)
        snapshot, app = snapshot_archive(source, private, args.destination)
        unpacked = private / "unpacked"
        unpacked.mkdir(mode=0o700)
        run("ditto", "-x", "-k", snapshot, unpacked)
        verify_bundle(unpacked / app, IOS if args.destination == "ios" else MAC, args.destination,
                      args.version, args.build_number, args.identity, profiles[IOS if args.destination == "ios" else MAC])
        if args.destination == "ios":
            verify_bundle(unpacked / app / "PlugIns/AlondraNotificationService.appex", EXT, "ios",
                          args.version, args.build_number, args.identity, profiles[EXT])
            artifact = output / "Alondra.ipa"
            shutil.copyfile(snapshot, artifact)
        else:
            artifact = output / "Alondra.pkg"
            # Recheck the exact valid identity immediately before signing.
            local_identity(args.installer_identity.upper(), installer=True)
            run("productbuild", "--component", unpacked / app, "/Applications", "--sign", installer, artifact)
            verify_pkg_trust(run("pkgutil", "--check-signature", artifact))
            verify_pkg_leaf(run("xar", "--dump-toc=-", "-f", artifact), args.installer_identity.upper())
    manifest = {"source": source_commit,
                "destination": args.destination, "version": args.version, "build": args.build_number,
                "artifact": artifact.name, "sha256": digest(artifact), "identity": args.identity,
                "installer_identity": args.installer_identity.upper() if args.installer_identity else None,
                "architecture": "arm64", "profile_sha256": profile_hashes}
    (output / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print("Verified local artifact: " + str(artifact) + ". Not uploaded; App Store processing and runtime gates remain.")


if __name__ == "__main__":
    try:
        main()
    except (GateError, OSError, ValueError, KeyError, zipfile.BadZipFile, plistlib.InvalidFileException, ET.ParseError) as error:
        # Fixed diagnostics only; do not print plist bodies or command arguments.
        print("Apple distribution blocked: " + (str(error) if isinstance(error, GateError) else type(error).__name__), file=sys.stderr)
        sys.exit(1)
