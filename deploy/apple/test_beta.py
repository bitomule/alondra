import base64
import datetime as dt
import hashlib
import json
from pathlib import Path
import tempfile
import types
import unittest
from unittest import mock

import beta


class BetaAdapterTests(unittest.TestCase):
    def test_version_is_read_from_bazel_distribution_definition(self):
        self.assertEqual(beta.version_from_build_file(), ("0.1.1", "3"))

    def test_notes_require_all_skill_locales_and_short_bullets(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "notes"
            path.write_text("[en-US]\n- Test inbox\n\n[de-DE]\n- Inbox testen\n\n[es-MX]\n- Prueba la bandeja\n")
            self.assertEqual(set(beta.read_notes(path)), set(beta.LOCALES))
            path.write_text("[en-US]\n- Only English\n")
            with self.assertRaisesRegex(beta.ReleaseError, "en-US, de-DE, and es-MX"):
                beta.read_notes(path)
            path.write_text("[en-US]\nTest inbox\n\n[de-DE]\n- Inbox testen\n\n[es-MX]\n- Prueba la bandeja\n")
            with self.assertRaisesRegex(beta.ReleaseError, "bullets"):
                beta.read_notes(path)

    def test_asc_profile_is_used_without_disclosing_profile_contents(self):
        with tempfile.TemporaryDirectory() as directory:
            home = Path(directory)
            (home / ".asc").mkdir()
            (home / ".asc/config.json").write_text('{"default_key_name":"desktop-key"}')
            self.assertEqual(beta.asc_prefix(home), ["asc", "--profile", "desktop-key"])
        self.assertEqual(beta.asc_prefix(Path("/nonexistent")), ["asc"])

    def test_next_build_number_accepts_asc_response_and_rejects_ambiguity(self):
        self.assertEqual(beta.next_build_number(b'{"data":{"nextBuildNumber":9}}'), 9)
        with self.assertRaises(beta.ReleaseError):
            beta.next_build_number(b'{"nextBuildNumber":9,"other":{"nextBuildNumber":10}}')

    def test_selected_distribution_certificate_must_match_and_have_14_days(self):
        certificate = b"synthetic DER bytes"
        identity = hashlib.sha1(certificate).hexdigest().upper()
        expiry = (dt.datetime.now(dt.timezone.utc) + dt.timedelta(days=30)).isoformat()
        payload = {"data": [{"attributes": {
            "certificateType": "DISTRIBUTION",
            "certificateContent": base64.b64encode(certificate).decode(),
            "expirationDate": expiry,
        }}]}
        self.assertEqual(beta.certificate_expiry(json.dumps(payload).encode(), identity).date(),
                         dt.datetime.fromisoformat(expiry).date())
        with self.assertRaisesRegex(beta.ReleaseError, "not found"):
            beta.certificate_expiry(json.dumps(payload).encode(), "0" * 40)

    def test_account_gate_rejects_agreement_blocker(self):
        beta.validate_account_status(b'{"data":{"healthy":true,"agreements":[]}}')
        with self.assertRaisesRegex(beta.ReleaseError, "blocker"):
            beta.validate_account_status(b'{"data":{"agreementStatus":"pending"}}')

    def test_build_selection_uses_build_number_and_platform_shape(self):
        rows = {"data": [
            {"id": "ios-4", "attributes": {"version": "4", "processingState": "VALID"}},
            {"id": "mac-4", "attributes": {"version": "4", "processingState": "VALID", "lsMinimumSystemVersion": "13.0"}},
        ]}
        encoded = json.dumps(rows).encode()
        self.assertEqual(beta.matching_build(encoded, build=4, macos=False)["id"], "ios-4")
        self.assertEqual(beta.matching_build(encoded, build=4, macos=True)["id"], "mac-4")
        with self.assertRaisesRegex(beta.ReleaseError, "multiple"):
            beta.matching_build(json.dumps({"data": [rows["data"][0], rows["data"][0]]}).encode(), build=4, macos=False)

    def test_group_lookup_requires_one_external_exact_match(self):
        rows = {"data": [
            {"id": "internal", "attributes": {"name": "QA", "isInternalGroup": True}},
            {"id": "external", "attributes": {"name": "QA", "isInternalGroup": False}},
        ]}
        self.assertEqual(beta.group_id(json.dumps(rows).encode(), "QA"), "external")
        rows["data"].append({"id": "external-2", "attributes": {"name": "QA", "isInternalGroup": False}})
        with self.assertRaises(beta.ReleaseError):
            beta.group_id(json.dumps(rows).encode(), "QA")

    def test_internal_group_lookup_requires_one_internal_exact_match(self):
        rows = {"data": [
            {"id": "internal", "attributes": {"name": "QA", "isInternalGroup": True}},
            {"id": "external", "attributes": {"name": "QA", "isInternalGroup": False}},
        ]}
        self.assertEqual(beta.group_id(json.dumps(rows).encode(), "QA", internal=True), "internal")
        rows["data"].pop(0)
        with self.assertRaisesRegex(beta.ReleaseError, "internal"):
            beta.group_id(json.dumps(rows).encode(), "QA", internal=True)

    def release_platform(self, destination, confirmed_groups):
        calls = []

        def fake_asc(*args):
            calls.append(args)
            if args[:3] == ("testflight", "groups", "list"):
                rows = [{"id": group, "name": group} for group in confirmed_groups]
                return json.dumps({"complete": True, "groups": rows, "groupCount": len(rows)}).encode()
            return b"{}"

        with tempfile.TemporaryDirectory() as directory:
            run = beta.BetaRun.__new__(beta.BetaRun)
            run.args = types.SimpleNamespace(output_root=Path(directory), identity="", installer_identity="",
                                             bazel_output_user_root=None, bazel_output_base=None)
            run.version = "1.0"
            run.source = "head"
            run.notes = {}
            run.result = {"version": "1.0", "platforms": {}}
            run.build_numbers = {destination: 5}
            run.groups = {destination: "external-id"}
            run.internal_groups = {destination: "internal-id"}
            run.call_asc = fake_asc
            run.wait_for_valid_build = lambda *_: {"id": "build-1"}
            artifact = {"manifest": {"sha256": "x"}, "artifact": Path("artifact")}
            with mock.patch.object(beta, "run"), \
                    mock.patch.object(beta, "manifest_ok", return_value=artifact), \
                    mock.patch.object(beta, "next_build_number", return_value=5), \
                    mock.patch.object(beta, "matching_build", return_value=None), \
                    mock.patch.object(beta, "upsert_notes"), \
                    mock.patch.object(beta, "validate_notes_response"):
                try:
                    run.release_platform(destination)
                finally:
                    self.calls = calls
                    self.result = run.result["platforms"][destination]

    def test_both_groups_are_added_on_ios_and_mac(self):
        for destination in ("ios", "macos"):
            with self.subTest(destination=destination):
                self.release_platform(destination, ["external-id", "internal-id"])
                add = next(call for call in self.calls if call[:2] == ("builds", "add-groups"))
                self.assertEqual(add[add.index("--group") + 1], "internal-id,external-id")
                self.assertEqual(self.result["state"], "distributed")
                self.assertEqual(self.result["internal_group_id"], "internal-id")

    def test_release_fails_when_either_group_is_missing_after_assignment(self):
        for destination in ("ios", "macos"):
            for confirmed in (["external-id"], ["internal-id"], []):
                with self.subTest(destination=destination, confirmed=confirmed):
                    with self.assertRaisesRegex(beta.ReleaseError, "group was not confirmed"):
                        self.release_platform(destination, confirmed)
                    self.assertNotIn(("testflight", "notifications"), [call[:2] for call in self.calls])

    def test_localized_notes_must_be_present_exactly_as_submitted(self):
        expected = {"en-US": "- Test one", "de-DE": "- Test zwei", "es-MX": "- Prueba tres"}
        response = {"data": [{"attributes": {"locale": locale, "whatsNew": text}}
                             for locale, text in expected.items()]}
        beta.validate_notes_response(json.dumps(response).encode(), expected)
        response["data"][0]["attributes"]["whatsNew"] = "- Different"
        with self.assertRaisesRegex(beta.ReleaseError, "do not match"):
            beta.validate_notes_response(json.dumps(response).encode(), expected)

    def test_notes_upsert_lists_existing_locales_and_selects_operation(self):
        calls = []
        def fake_asc(*args):
            calls.append(args)
            if args[:3] == ("builds", "test-notes", "list"):
                return b'{"data":[{"attributes":{"locale":"en-US"}}]}'
            return b"{}"

        notes = {"en-US": "- Update", "de-DE": "- Create"}
        operations = beta.upsert_notes(fake_asc, "build-1", notes)
        self.assertEqual(operations, {"en-US": "update", "de-DE": "create"})
        self.assertEqual(calls[0][:3], ("builds", "test-notes", "list"))
        self.assertEqual(calls[1][:3], ("builds", "test-notes", "update"))
        self.assertEqual(calls[2][:3], ("builds", "test-notes", "create"))

    def test_transient_update_failure_does_not_try_create(self):
        calls = []
        def failing_asc(*args):
            calls.append(args)
            if args[:3] == ("builds", "test-notes", "list"):
                return b'{"data":[{"attributes":{"locale":"en-US"}}]}'
            raise beta.ReleaseError("transient API error")

        with self.assertRaisesRegex(beta.ReleaseError, "update What to Test locale en-US"):
            beta.upsert_notes(failing_asc, "build-1", {"en-US": "- Update"})
        self.assertEqual([call[2] for call in calls], ["list", "update"])

    def test_group_membership_is_checked_by_group_id(self):
        self.assertEqual(beta.group_ids(b'{"data":[{"id":"group-one"}]}'), {"group-one"})
        current_cli_response = {
            "buildId": "build-12",
            "appId": "6816262826",
            "complete": True,
            "groupCount": 1,
            "groups": [{"id": "group-one", "name": "External QA", "type": "EXTERNAL",
                        "membership": "INVITED"}],
        }
        self.assertEqual(beta.group_ids(json.dumps(current_cli_response).encode()), {"group-one"})
        with self.assertRaises(beta.ReleaseError):
            beta.group_ids(b'{"data":null}')
        for response in (
            {**current_cli_response, "complete": False},
            {**current_cli_response, "groupCount": 2},
            {**current_cli_response, "groups": [{"name": "Missing ID"}]},
            {**current_cli_response, "groups": [current_cli_response["groups"][0]] * 2,
             "groupCount": 2},
        ):
            with self.subTest(response=response), self.assertRaises(beta.ReleaseError):
                beta.group_ids(json.dumps(response).encode())


if __name__ == "__main__":
    unittest.main()
