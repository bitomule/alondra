"""Exercise actual ZIP/plist gates without Xcode builds, signing or network."""
import copy
import base64
import shutil
import datetime as dt
import hashlib
from pathlib import Path
import plistlib
import tempfile
import unittest
import zipfile

from prepare import GateError, IOS, EXT, MAC, TEAM, validate_archive, validate_profile, snapshot_archive, verify_pkg_leaf, require_installer_unique


class DistributionInputTests(unittest.TestCase):
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
        mac = self.profile(MAC, "OSX")
        mac["Entitlements"]["com.apple.application-identifier"] = mac["Entitlements"].pop("application-identifier")
        validate_profile(mac, MAC, "macos", identity)

    def archive(self, path, simulator=False, extension=True, extra=None):
        app = "Payload/Alondra.app/"
        with zipfile.ZipFile(path, "w") as archive:
            archive.writestr(app + "Info.plist", plistlib.dumps({
                "DTPlatformName": "iphonesimulator" if simulator else "iphoneos",
                "CFBundleSupportedPlatforms": ["iPhoneSimulator" if simulator else "iPhoneOS"],
            }))
            if extension:
                archive.writestr(app + "PlugIns/AlondraNotificationService.appex/Info.plist", plistlib.dumps({}))
            if extra:
                archive.writestr(*extra)

    def test_ipa_gate_rejects_simulator_missing_extension_and_foreign_payload(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.ipa"
            self.archive(path)
            self.assertEqual(validate_archive(path, "ios"), "Payload/Alondra.app")
            for options in ({"simulator": True}, {"extension": False},
                            {"extra": ("credentials.json", "private")},
                            {"extra": ("../outside", "escape")},
                            {"extra": ("Payload/Other.app/Info.plist", "foreign")},
                            {"extra": ("Payload/Alondra.app/PlugIns/Other.appex/Info.plist", "foreign")}):
                with self.subTest(options=options):
                    self.archive(path, **options)
                    with self.assertRaises(GateError):
                        validate_archive(path, "ios")

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
        other = hashlib.sha1(wrong).hexdigest().upper()
        name = "3rd Party Mac Developer Installer: Fixture (658YBSK6MG)"
        with self.assertRaisesRegex(GateError, "ambiguous"):
            require_installer_unique(f'1) {fingerprint} "{name}"\n2) {other} "{name}"', fingerprint, name)
        require_installer_unique(f'1) {fingerprint} "{name}"', fingerprint, name)
        def toc(certificates):
            values = "".join("<X509Certificate>" + base64.b64encode(cert).decode() + "</X509Certificate>" for cert in certificates)
            return '<xar><toc><signature><KeyInfo xmlns="http://www.w3.org/2000/09/xmldsig#"><X509Data>' + values + '</X509Data></KeyInfo></signature></toc></xar>'
        verify_pkg_leaf(toc([requested, wrong]), fingerprint)
        with self.assertRaisesRegex(GateError, "Actual Installer leaf"):
            verify_pkg_leaf(toc([wrong, requested]), fingerprint)
        with self.assertRaisesRegex(GateError, "Missing Installer leaf"):
            verify_pkg_leaf(toc([]), fingerprint)

    def test_archive_rejects_symlink_escape_before_ditto(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "input.ipa"
            link = zipfile.ZipInfo("Payload/Alondra.app/Link")
            link.create_system = 3
            link.external_attr = 0o120777 << 16
            self.archive(path, extra=(link, "../../../outside"))
            with self.assertRaisesRegex(GateError, "symlink"):
                validate_archive(path, "ios")


if __name__ == "__main__":
    unittest.main()
