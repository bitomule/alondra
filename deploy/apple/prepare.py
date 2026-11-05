#!/usr/bin/env python3
"""Build and verify local Apple distribution artifacts; never upload or modify ASC."""
import argparse
import base64
import datetime as dt
import gzip
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
SIGNING_DIR = Path(os.environ.get("ALONDRA_SIGNING_DIR") or Path.home() / ".alondra" / "signing")
TEAM = "658YBSK6MG"
IOS = "com.bitomule.alondra"
MAC = IOS
EXT = IOS + ".NotificationService"
LIVE_ACTIVITY = IOS + ".LiveActivity"
# Every iOS app extension, by bundle ID, with the bundle name it has under PlugIns/.
IOS_EXTENSIONS = {EXT: "AlondraNotificationService", LIVE_ACTIVITY: "AlondraLiveActivity"}
VERSIONS = ROOT / "tools/shared.bzl"
BETA_OUTPUT = ROOT / ".beta-build"
# The /beta skill writes its release notes here and deletes them after the upload.
BETA_NOTES = "?? .beta_notes"
PROFILES = {
    "ios": [(IOS, "Alondra_AppStore.mobileprovision"), (EXT, "Alondra_NotificationService_AppStore.mobileprovision"),
            (LIVE_ACTIVITY, "Alondra_LiveActivity_AppStore.mobileprovision")],
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
        if Path(str(args[0])).name in {"bazelisk", "codesign", "productbuild", "pkgutil", "xcrun", "lsbom", "mkbom"}:
            text += result.stdout.decode(errors="replace")
        text = re.sub(r"(?im)^.*(?:authorization|bearer |password|token|secret|api.?key|remote_header).*$", "[redacted]", text)
        with log.open("x") as stream:
            os.chmod(log, 0o600)
            stream.write(f"{Path(str(args[0])).name}: exit {result.returncode}\n" + text[-65536:])
        raise GateError(f"{args[0]} failed (exit {result.returncode}); artifact not approved. Private diagnostic: {log}")
    return result.stdout


def read_versions(path=VERSIONS):
    source = path.read_text()
    version = re.search(r'^\s*version = "([0-9]+\.[0-9]+\.[0-9]+)"', source, re.MULTILINE)
    build = re.search(r'^\s*build_number = "([1-9][0-9]{0,3})"', source, re.MULTILINE)
    require(version and build, "Could not read version and build_number from tools/shared.bzl")
    return version.group(1), build.group(1)


def uncommitted(status):
    return [line for line in status.decode().splitlines() if line and line != BETA_NOTES]


def prepare_output(output, root=ROOT, beta_output=BETA_OUTPUT):
    output, root, beta_output = output.resolve(), root.resolve(), beta_output.resolve()
    if output.is_relative_to(beta_output) and output != beta_output:
        if output.exists():
            shutil.rmtree(output)
    else:
        require(not output.exists() and not output.is_relative_to(root),
                "Output must be under .beta-build/ or a new directory outside checkout")
    output.mkdir(parents=True, mode=0o700)
    return output


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
    if bundle_id == IOS and destination == "ios":
        require(ent.get("aps-environment") == "production", "APNs distribution profile must use production")
    return ent


def only_identity(output, installer=False):
    required = "3rd Party Mac Developer Installer:" if installer else "Apple Distribution:"
    matches = {value for value, name in re.findall(r'([0-9A-F]{40}) "([^"]+)"', output)
               if name.startswith(required) and name.endswith("(" + TEAM + ")")}
    require(len(matches) == 1, "Expected exactly one valid " + required.rstrip(":") + " identity for team " + TEAM
            + "; pass it explicitly")
    return matches.pop()


def default_identity(installer=False):
    return only_identity(run("security", "find-identity", "-v", "-p", "basic" if installer else "codesigning").decode(), installer)


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


def normalize_bundle(bundle):
    """Make the unpacked app readable by everyone.

    Mode bits are outside the code signature, so this does not invalidate it."""
    for directory, directories, files in os.walk(bundle):
        for name in directories + files:
            path = Path(directory) / name
            if path.is_symlink():
                os.chmod(path, 0o755, follow_symlinks=False)
            elif path.is_dir():
                os.chmod(path, 0o755)
            else:
                os.chmod(path, 0o755 if path.stat().st_mode & 0o111 else 0o644)
    os.chmod(bundle, 0o755)


def is_appledouble(path):
    return PurePosixPath(path).name.startswith("._")


def cpio_entries(payload):
    """(name, bytes) of each entry of an odc cpio stream, its trailer included."""
    offset = 0
    while True:
        header = payload[offset:offset + 76]
        require(len(header) == 76 and header[:6] == b"070707", "Installer payload is not an odc cpio archive")
        name_size, file_size = int(header[59:65], 8), int(header[65:76], 8)
        name = payload[offset + 76:offset + 76 + name_size].rstrip(b"\0").decode()
        end = offset + 76 + name_size + file_size
        require(end <= len(payload), "Truncated installer payload")
        yield name, payload[offset:end]
        offset = end
        if name == "TRAILER!!!":
            return


def read_payload(path):
    compressed = path.read_bytes()
    require(compressed[:2] == b"\x1f\x8b", "Installer payload is not gzip-compressed")
    return gzip.decompress(compressed)


def strip_appledouble(component):
    """Drop the ._ entries productbuild archives for extended attributes from Payload and Bom.

    macOS gives every file a node creates com.apple.provenance, which cannot be removed, so
    stripping attributes from the app beforehand does not stop them."""
    payload = component / "Payload"
    entries = cpio_entries(read_payload(payload))
    payload.write_bytes(gzip.compress(b"".join(data for name, data in entries if not is_appledouble(name)), mtime=0))
    bom = component / "Bom"
    listing = component.parent / (component.name + ".bom.txt")
    # productbuild writes the root with mode 0, which mkbom -i cannot read; the payload's root is 40755.
    kept = [".\t40755\t0/0" if line == ".\t0\t0/0" else line
            for line in run("lsbom", bom).decode().splitlines() if not is_appledouble(line.split("\t", 1)[0])]
    listing.write_text("".join(line + "\n" for line in kept))
    # mkbom -i skips the lines it cannot insert and still exits 0.
    run("mkbom", "-i", listing, bom)
    listing.unlink()
    require(run("lsbom", bom).decode().splitlines() == kept, "Rebuilt installer Bom differs from its listing")


def stripped_components(app, directory):
    """productbuild the app's pkg; return its Distribution and a folder with its components without AppleDouble entries."""
    unsigned = directory / "unsigned.pkg"
    run("productbuild", "--component", app, "/Applications", unsigned)
    expanded = directory / "expanded"
    run("pkgutil", "--expand", unsigned, expanded)
    components = sorted(path for path in expanded.iterdir() if path.suffix == ".pkg")
    require(components, "Installer package has no component")
    packages = directory / "packages"
    packages.mkdir()
    for component in components:
        strip_appledouble(component)
        run("pkgutil", "--flatten", component, packages / component.name)
    return expanded / "Distribution", packages


def bom_problems(listing):
    problems = []
    for line in listing.splitlines():
        path, _, mode = line.rpartition("\t")
        mode = mode.strip()
        if path == ".":
            continue
        if is_appledouble(path):
            problems.append("AppleDouble entry: " + path)
        elif len(mode) == 10 and (mode[7] != "r" or (mode[0] == "d" and mode[9] != "x")):
            problems.append(mode + " " + path)
    return problems


def pkg_problems(pkg):
    with tempfile.TemporaryDirectory(prefix="alondra-pkg-bom-") as directory:
        expanded = Path(directory) / "expanded"
        run("pkgutil", "--expand", pkg, expanded)
        boms = sorted(expanded.rglob("Bom"))
        require(boms, "Installer package has no Bom")
        return ([problem for bom in boms for problem in bom_problems(run("lsbom", "-p", "fM", bom).decode())]
                + ["AppleDouble payload entry: " + name for bom in boms
                   for name, _ in cpio_entries(read_payload(bom.parent / "Payload")) if is_appledouble(name)])


def verify_pkg_contents(pkg):
    """Refuse the package App Store Connect rejects: ITMS-90255 (root-only entries) and AppleDouble files in Bom or Payload."""
    problems = pkg_problems(pkg)
    require(not problems, "Installer package would be rejected by App Store Connect: " + "; ".join(problems[:5]))


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
        expected = sorted(f"{app}/PlugIns/{name}.appex" for name in IOS_EXTENSIONS.values()) if destination == "ios" else []
        require(extensions == expected, "Missing or unexpected app extension")
        return app


def validate_store_metadata(info, bundle_id, destination):
    expected_type = "XPC!" if bundle_id in IOS_EXTENSIONS else "APPL"
    require(info.get("CFBundlePackageType") == expected_type, "App Store bundle package type mismatch")
    if destination == "ios":
        capabilities = info.get("UIRequiredDeviceCapabilities", [])
        has_arm64 = (isinstance(capabilities, list) and "arm64" in capabilities
                     or isinstance(capabilities, dict) and capabilities.get("arm64") is True)
        require(has_arm64, "App Store arm64 device capability missing")


def verify_bundle(bundle, bundle_id, destination, version, build, identity, profile_path):
    content = bundle if destination == "ios" else bundle / "Contents"
    info = read_plist(content / "Info.plist")
    validate_store_metadata(info, bundle_id, destination)
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
        if bundle_id in (IOS, LIVE_ACTIVITY):
            require(ent.get("com.apple.security.application-groups") == ["group." + IOS], "Signed App Group mismatch")
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
    parser.add_argument("--identity", help="Apple Distribution SHA-1; defaults to the only valid one in the keychain")
    parser.add_argument("--installer-identity", help="Mac Installer SHA-1 (macos only); defaults to the only valid one")
    parser.add_argument("--output", required=True, type=Path,
                        help="Directory under .beta-build/ (replaced) or a new directory outside checkout")
    parser.add_argument("--bazel-output-user-root", type=Path, help="Reuse the coordinated local Bazel storage root")
    parser.add_argument("--bazel-output-base", type=Path, help="Reuse the coordinated local Bazel output base")
    parser.add_argument("--preflight-only", action="store_true", help="Validate existing local signing inputs without building")
    args = parser.parse_args()
    os.umask(0o022)
    os.chdir(ROOT)
    args.version, args.build_number = read_versions()
    args.identity = (args.identity or default_identity()).upper()
    signing_name = local_identity(args.identity)
    installer = None
    if args.destination == "macos":
        args.installer_identity = args.installer_identity or default_identity(installer=True)
        installer = local_identity(args.installer_identity.upper(), installer=True)
    profiles = {}
    profile_hashes = {}
    for bundle_id, filename in PROFILES[args.destination]:
        path = SIGNING_DIR / filename
        require(path.is_file(), "Missing profile: " + str(path))
        profile = plistlib.loads(run("security", "cms", "-D", "-i", path))
        validate_profile(profile, bundle_id, args.destination, args.identity)
        profiles[bundle_id] = path
        profile_hashes[bundle_id] = digest(path)
    require(not uncommitted(run("git", "-C", ROOT, "status", "--porcelain", "--untracked-files=normal")), "Use a clean committed checkout")
    source_commit = run("git", "-C", ROOT, "rev-parse", "HEAD").decode().strip()
    if args.preflight_only:
        print("Local profile/identity preflight passed; no build or publication performed.")
        return
    output = prepare_output(args.output)
    target = "//apple:AlondraiOS" if args.destination == "ios" else "//apple:AlondraMac"
    flags = ["--define=alondra_distribution=1",
             # rules_apple resolves this flag by certificate common name, not SHA-1.
             # Bundle verification below still requires the exact requested leaf.
             "--@build_bazel_rules_apple//apple/build_settings:signing_certificate_name=" + signing_name,
             "--ios_multi_cpus=arm64" if args.destination == "ios" else "--macos_cpus=arm64",
             "-c", "opt"]
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
            and not uncommitted(run("git", "-C", ROOT, "status", "--porcelain", "--untracked-files=normal")), "Source changed during build")
    with tempfile.TemporaryDirectory(prefix="alondra-package-", dir=output) as directory:
        private = Path(directory)
        snapshot, app = snapshot_archive(source, private, args.destination)
        unpacked = private / "unpacked"
        unpacked.mkdir(mode=0o700)
        run("ditto", "-x", "-k", "--norsrc", "--noextattr", snapshot, unpacked)
        if args.destination == "macos":
            normalize_bundle(unpacked / app)
        verify_bundle(unpacked / app, IOS if args.destination == "ios" else MAC, args.destination,
                      args.version, args.build_number, args.identity, profiles[IOS if args.destination == "ios" else MAC])
        if args.destination == "ios":
            for bundle_id, name in IOS_EXTENSIONS.items():
                verify_bundle(unpacked / app / f"PlugIns/{name}.appex", bundle_id, "ios",
                              args.version, args.build_number, args.identity, profiles[bundle_id])
            artifact = output / "Alondra.ipa"
            shutil.copyfile(snapshot, artifact)
        else:
            artifact = output / "Alondra.pkg"
            distribution, packages = stripped_components(unpacked / app, private)
            # Recheck the exact valid identity immediately before signing.
            local_identity(args.installer_identity.upper(), installer=True)
            run("productbuild", "--distribution", distribution, "--package-path", packages, "--sign", installer, artifact)
            verify_pkg_contents(artifact)
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
