"""Exercise actual ZIP/plist gates without Xcode builds, signing or network."""
import copy
import datetime as dt
import hashlib
from pathlib import Path
import plistlib
import tempfile
import unittest
import zipfile

from prepare import GateError, IOS, EXT, MAC, TEAM, validate_archive, validate_profile


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
