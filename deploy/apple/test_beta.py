import base64
import datetime as dt
import hashlib
import json
from pathlib import Path
import tempfile
import unittest

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
        with self.assertRaises(beta.ReleaseError):
            beta.group_ids(b'{"data":null}')


if __name__ == "__main__":
    unittest.main()
