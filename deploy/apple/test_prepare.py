#!/usr/bin/env python3
"""Exercise actual ZIP/plist gates without Xcode builds, signing or network."""
import copy
import base64
import shutil
import datetime as dt
import hashlib
import os
import subprocess
from pathlib import Path
import plistlib
import tempfile
import unittest
import zipfile

from prepare import (GateError, IOS, EXT, LIVE_ACTIVITY, MAC, PROFILES, TEAM, only_identity, prepare_output, read_versions, uncommitted,
                     validate_archive, validate_profile, snapshot_archive, verify_pkg_leaf, verify_pkg_trust,
                     validate_store_metadata, normalize_bundle, bom_problems, pkg_problems, verify_pkg_contents,
                     stripped_components)


class DistributionInputTests(unittest.TestCase):
    def test_store_metadata_rejects_observed_asc_failures_in_app_and_extension(self):
        # ASC rejected the real first IPA with 90183 (missing APPL) and
        # 90502 (missing arm64 in both app and extension).
        for bundle, kind in [(IOS, "APPL"), (EXT, "XPC!"), (LIVE_ACTIVITY, "XPC!")]:
            valid = {"CFBundlePackageType": kind, "UIRequiredDeviceCapabilities": ["arm64"]}
            validate_store_metadata(plistlib.loads(plistlib.dumps(valid)), bundle, "ios")
            for key in valid:
                broken = {k: v for k, v in valid.items() if k != key}
                with self.subTest(bundle=bundle, missing=key), self.assertRaises(GateError):
                    validate_store_metadata(broken, bundle, "ios")
            with self.assertRaises(GateError):
                validate_store_metadata({"CFBundlePackageType": kind,
                                         "UIRequiredDeviceCapabilities": {"arm64": False}}, bundle, "ios")
        validate_store_metadata({"CFBundlePackageType": "APPL"}, MAC, "macos")
        with self.assertRaises(GateError):
            validate_store_metadata({}, MAC, "macos")

    def profile(self, bundle=IOS, platform="iOS"):
        return {
            "ExpirationDate": dt.datetime(2030, 1, 1),
            "TeamIdentifier": [TEAM], "ApplicationIdentifierPrefix": [TEAM],
            "Platform": [platform], "DeveloperCertificates": [b"fixture public certificate"],
            "Entitlements": {"application-identifier": TEAM + "." + bundle, "get-task-allow": False,
                             "aps-environment": "production"},
        }

    def test_distribution_profile_checks_real_plist_inputs(self):
        identity = hashlib.sha1(b"fixture public certificate").hexdigest().upper()
        valid = self.profile()
        # Serialize through the same plist parser used for security cms output.
        validate_profile(plistlib.loads(plistlib.dumps(valid)), IOS, "ios", identity)
        changes = [
            ("ExpirationDate", dt.datetime(2020, 1, 1)),
            ("ProvisionedDevices", ["a registered device"]),
            ("ProvisionsAllDevices", True),
            ("TeamIdentifier", ["OTHERTEAM"]),
            ("ApplicationIdentifierPrefix", ["OLDPREFIX"]),
            ("DeveloperCertificates", [b"different certificate"]),
            ("Platform", ["OSX"]),
        ]
        for key, value in changes:
            with self.subTest(key=key):
                invalid = copy.deepcopy(valid)
                invalid[key] = value
                with self.assertRaises(GateError):
                    validate_profile(plistlib.loads(plistlib.dumps(invalid)), IOS, "ios", identity)
        for key, value in (("application-identifier", TEAM + "." + EXT),
                           ("aps-environment", "development"), ("get-task-allow", True)):
            with self.subTest(entitlement=key):
                invalid = copy.deepcopy(valid)
                invalid["Entitlements"][key] = value
                with self.assertRaises(GateError):
                    validate_profile(invalid, IOS, "ios", identity)
        # The Mac shares the iOS App ID, so its profile carries push under the macOS key and no
        # aps-environment; the Mac app does not use push.
        mac = self.profile(MAC, "OSX")
        mac["Entitlements"]["com.apple.application-identifier"] = mac["Entitlements"].pop("application-identifier")
        mac["Entitlements"]["com.apple.developer.aps-environment"] = mac["Entitlements"].pop("aps-environment")
        validate_profile(mac, MAC, "macos", identity)
        ios_without_push = self.profile()
        del ios_without_push["Entitlements"]["aps-environment"]
        with self.assertRaisesRegex(GateError, "APNs"):
            validate_profile(ios_without_push, IOS, "ios", identity)

    def archive(self, path, simulator=False, extensions=("AlondraNotificationService", "AlondraLiveActivity"), extra=None):
        app = "Payload/Alondra.app/"
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr(app + "Info.plist", plistlib.dumps({
                "DTPlatformName": "iphonesimulator" if simulator else "iphoneos",
                "CFBundleSupportedPlatforms": ["iPhoneSimulator" if simulator else "iPhoneOS"],
            }))
            for extension in extensions:
                archive.writestr(app + f"PlugIns/{extension}.appex/Info.plist", plistlib.dumps({}))
            if extra:
                archive.writestr(*extra)

    def test_ipa_gate_rejects_simulator_missing_extension_and_foreign_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.ipa"
            self.archive(path)
            self.assertEqual(validate_archive(path, "ios"), "Payload/Alondra.app")
            for options in ({"simulator": True}, {"extensions": ()},
                            {"extensions": ("AlondraNotificationService",)},
                            {"extensions": ("AlondraLiveActivity",)},
                            {"extra": ("credentials.json", "private")},
                            {"extra": ("../outside", "escape")},
                            {"extra": ("Payload/Other.app/Info.plist", "foreign")},
                            {"extra": ("Payload/Alondra.app/PlugIns/Other.appex/Info.plist", "foreign")}):
                with self.subTest(options=options):
                    self.archive(path, **options)
                    with self.assertRaises(GateError):
                        validate_archive(path, "ios")

    def test_the_ios_release_needs_a_store_profile_for_each_extension(self):
        self.assertEqual([bundle for bundle, _ in PROFILES["ios"]], [IOS, EXT, LIVE_ACTIVITY])
        self.assertIn((LIVE_ACTIVITY, "Alondra_LiveActivity_AppStore.mobileprovision"), PROFILES["ios"])

    def test_replaced_bazel_output_cannot_change_verified_snapshot_or_shipped_ipa(self):
        with tempfile.TemporaryDirectory() as directory:
            directory = Path(directory)
            source = directory / "bazel.ipa"
            private = directory / "private"
            private.mkdir()
            self.archive(source)
            original_bytes = source.read_bytes()
            snapshot, app = snapshot_archive(source, private, "ios")
            self.assertEqual(app, "Payload/Alondra.app")
            # A second Bazel build replaces the path after validation, before copy.
            source.unlink()
            self.archive(source, simulator=True)
            with self.assertRaises(GateError):
                validate_archive(source, "ios")
            shipped = directory / "release.ipa"
            shutil.copyfile(snapshot, shipped)
            self.assertEqual(shipped.read_bytes(), original_bytes)
            self.assertEqual(validate_archive(shipped, "ios"), app)
            self.assertEqual(snapshot.stat().st_mode & 0o777, 0o400)

    def test_same_common_name_does_not_authorize_another_installer_leaf(self):
        # The signature TOC's DER bytes decide identity, not a displayed CN or a
        # requested certificate occurring later in the chain.
        requested = b"first Installer public DER fixture"
        wrong = b"other Installer public DER fixture with identical common name"
        fingerprint = hashlib.sha1(requested).hexdigest().upper()
        def toc(certificates):
            values = "".join("<X509Certificate>" + base64.b64encode(cert).decode() + "</X509Certificate>" for cert in certificates)
            return '<xar><toc><signature><KeyInfo xmlns="http://www.w3.org/2000/09/xmldsig#"><X509Data>' + values + '</X509Data></KeyInfo></signature></toc></xar>'
        verify_pkg_leaf(toc([requested, wrong]), fingerprint)
        with self.assertRaisesRegex(GateError, "Actual Installer leaf"):
            verify_pkg_leaf(toc([wrong, requested]), fingerprint)
        with self.assertRaisesRegex(GateError, "Missing Installer leaf"):
            verify_pkg_leaf(toc([]), fingerprint)

    def test_pkgutil_success_exit_does_not_imply_certificate_trust(self):
        # Observed from the same real signed package inside the restricted sandbox:
        # pkgutil returned zero while printing this untrusted status.
        with self.assertRaisesRegex(GateError, "trust not established"):
            verify_pkg_trust(b'Package "signed.pkg":\n   Status: signed by untrusted certificate\n')
        verify_pkg_trust(b'Package "signed.pkg":\n   Status: signed by a developer certificate issued by Apple (Development)\n')
        with self.assertRaises(GateError):
            verify_pkg_trust(b'No status available')

    def test_archive_rejects_symlink_escape_before_ditto(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.ipa"
            link = zipfile.ZipInfo("Payload/Alondra.app/Link")
            link.create_system = 3
            link.external_attr = 0o120777 << 16
            self.archive(path, extra=(link, "../../../outside"))
            with self.assertRaisesRegex(GateError, "symlink"):
                validate_archive(path, "ios")


class BetaInputTests(unittest.TestCase):
    def test_version_and_build_come_from_the_shared_file(self):
        self.assertRegex(read_versions()[0], r"^[0-9]+\.[0-9]+\.[0-9]+$")
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "shared.bzl"
            path.write_text('versions = struct(\n    minimum_ios_version = "26.0",\n    version = "0.3.0",\n    build_number = "39",\n)\n')
            self.assertEqual(read_versions(path), ("0.3.0", "39"))
            path.write_text('versions = struct(\n    version = "0.3",\n    build_number = "39",\n)\n')
            with self.assertRaisesRegex(GateError, "tools/shared.bzl"):
                read_versions(path)

    def test_only_the_beta_notes_may_be_untracked(self):
        self.assertEqual(uncommitted(b"?? .beta_notes\n"), [])
        self.assertEqual(uncommitted(b"?? .beta_notes\n M apple/BUILD.bazel\n"), [" M apple/BUILD.bazel"])
        self.assertEqual(uncommitted(b"?? notes/.beta_notes\n"), ["?? notes/.beta_notes"])

    def test_beta_output_is_replaced_and_other_checkout_paths_refused(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / "repo"
            beta = root / ".beta-build"
            stale = beta / "ios" / "Alondra.ipa"
            stale.parent.mkdir(parents=True)
            stale.write_text("previous build")
            self.assertEqual(prepare_output(beta / "ios", root, beta), (beta / "ios").resolve())
            self.assertFalse(stale.exists())
            for refused in (beta, root / "apple"):
                with self.assertRaisesRegex(GateError, "Output must be"):
                    prepare_output(refused, root, beta)
            outside = Path(directory) / "outside"
            self.assertEqual(prepare_output(outside, root, beta), outside.resolve())
            with self.assertRaisesRegex(GateError, "Output must be"):
                prepare_output(outside, root, beta)

    def test_default_identity_must_be_unique_for_the_team(self):
        one = '  1) ' + 'A' * 40 + ' "Apple Distribution: David (' + TEAM + ')"\n'
        other_team = '  2) ' + 'B' * 40 + ' "Apple Distribution: Someone (XXXXXXXXXX)"\n'
        installer = '  3) ' + 'C' * 40 + ' "3rd Party Mac Developer Installer: David (' + TEAM + ')"\n'
        self.assertEqual(only_identity(one + other_team + installer), "A" * 40)
        self.assertEqual(only_identity(one + installer, installer=True), "C" * 40)
        with self.assertRaisesRegex(GateError, "exactly one"):
            only_identity(one + one.replace("A" * 40, "D" * 40))
        with self.assertRaisesRegex(GateError, "exactly one"):
            only_identity(other_team)


class PackageContentsTests(unittest.TestCase):
    def build_app(self, directory, normalize):
        previous = os.umask(0o077)
        try:
            root = Path(directory) / "root"
            app = root / "Alondra.app"
            (app / "Contents/MacOS").mkdir(parents=True)
            (app / "Contents/Info.plist").write_bytes(plistlib.dumps({"CFBundleExecutable": "Alondra", "CFBundleIdentifier": "test.alondra",
                                                                     "CFBundleShortVersionString": "1.0", "CFBundleVersion": "1"}))
            (app / "Contents/MacOS/Alondra").write_text("#!/bin/sh\n")
            os.chmod(app / "Contents/MacOS/Alondra", 0o700)
            (app / "Contents/Frameworks/Kit.framework/Versions/A").mkdir(parents=True)
            (app / "Contents/Frameworks/Kit.framework/Versions/A/Kit").write_text("kit\n")
            os.symlink("A", app / "Contents/Frameworks/Kit.framework/Versions/Current")
            # Stands in for com.apple.provenance, which macOS puts on every file a node writes.
            subprocess.run(["xattr", "-w", "com.apple.quarantine", "0081;0;x;", app / "Contents/Info.plist"], check=True)
            if normalize:
                normalize_bundle(app)
                os.chmod(root, 0o755)
            return root, app
        finally:
            os.umask(previous)

    def test_bom_listing_flags_private_modes_and_appledouble_entries(self):
        listing = "\n".join(["./Alondra.app\tdrwxr-xr-x", "./Alondra.app/Contents\tdrwx------",
                              "./Alondra.app/Contents/Info.plist\t-rw-r--r--", "./Alondra.app/Contents/Secret\t-rw-------",
                              "./Alondra.app/Contents/._Info.plist\t-rw-r--r--", "./Alondra.app/Locked\tdrw-r--r--"])
        self.assertEqual(len(bom_problems(listing)), 4)
        self.assertEqual(bom_problems(".\t?---------\n./Alondra.app\tdrwxr-xr-x\n./Alondra.app/x\t-rwxr-xr-x"), [])

    def test_package_built_under_a_private_umask_is_refused_until_normalized(self):
        with tempfile.TemporaryDirectory() as directory:
            root, _ = self.build_app(directory, normalize=False)
            pkg = Path(directory) / "Alondra.pkg"
            subprocess.run(["pkgbuild", "--root", root, "--identifier", "test.alondra", "--install-location", "/Applications", pkg],
                           check=True, capture_output=True)
            self.assertTrue(any(p.startswith("drwx------") for p in pkg_problems(pkg)))
            with self.assertRaises(GateError):
                verify_pkg_contents(pkg)
        with tempfile.TemporaryDirectory() as directory:
            _, app = self.build_app(directory, normalize=True)
            self.assertEqual((app / "Contents").stat().st_mode & 0o777, 0o755)
            self.assertEqual((app / "Contents/Info.plist").stat().st_mode & 0o777, 0o644)
            self.assertEqual((app / "Contents/MacOS/Alondra").stat().st_mode & 0o777, 0o755)

    def test_package_of_an_app_with_extended_attributes_has_no_appledouble_entries(self):
        with tempfile.TemporaryDirectory() as directory:
            _, app = self.build_app(directory, normalize=True)
            productbuild = Path(directory) / "productbuild.pkg"
            subprocess.run(["productbuild", "--component", app, "/Applications", productbuild], check=True, capture_output=True)
            self.assertIn("AppleDouble payload entry: ./Alondra.app/Contents/._Info.plist", pkg_problems(productbuild))
            work = Path(directory) / "work"
            work.mkdir()
            distribution, packages = stripped_components(app, work)
            pkg = Path(directory) / "Alondra.pkg"
            subprocess.run(["productbuild", "--distribution", distribution, "--package-path", packages, pkg], check=True, capture_output=True)
            self.assertEqual(pkg_problems(pkg), [])
            verify_pkg_contents(pkg)
            files = subprocess.run(["pkgutil", "--payload-files", pkg], check=True, capture_output=True, text=True).stdout.split()
            self.assertIn("./Alondra.app/Contents/Frameworks/Kit.framework/Versions/Current", files)
            self.assertFalse([f for f in files if "/._" in f])


if __name__ == "__main__":
    unittest.main()
