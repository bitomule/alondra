#!/usr/bin/env python3
"""The BuildBuddy credential helper against a fake `security`, never the real Keychain."""
import json
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

helper = Path(__file__).resolve().parents[1] / "buildbuddy-credential-helper.sh"

FAKE_SECURITY = """#!/bin/bash
[ "$1" = find-generic-password ] || exit 64
[ "$3" = "$EXPECTED_SERVICE" ] && [ "$5" = "$EXPECTED_ACCOUNT" ] || exit 44
[ -f "$FAKE_KEY_FILE" ] || exit 44
cat "$FAKE_KEY_FILE"
"""


class CredentialHelperTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.addCleanup(self.directory.cleanup)
        root = Path(self.directory.name)
        bin_dir = root / "bin"
        bin_dir.mkdir()
        security = bin_dir / "security"
        security.write_text(FAKE_SECURITY)
        security.chmod(0o755)
        self.key_file = root / "key"
        self.env = {
            "PATH": f"{bin_dir}:/usr/bin:/bin",
            "USER": "tester",
            "EXPECTED_SERVICE": "buildbuddy-api-key",
            "EXPECTED_ACCOUNT": "tester",
            "FAKE_KEY_FILE": str(self.key_file),
        }

    def run_helper(self, *args):
        return subprocess.run(
            ["/bin/bash", str(helper), *args],
            input='{"uri":"https://remote.buildbuddy.io"}',
            capture_output=True,
            text=True,
            env=self.env,
        )

    def test_get_returns_the_keychain_key_as_the_buildbuddy_header(self):
        self.key_file.write_text("AbC123xyz\n")
        result = self.run_helper("get")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(
            json.loads(result.stdout),
            {"headers": {"x-buildbuddy-api-key": ["AbC123xyz"]}},
        )
        self.assertEqual(result.stderr, "")

    def test_missing_keychain_item_fails_without_output(self):
        result = self.run_helper("get")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertIn("no Keychain item 'buildbuddy-api-key'", result.stderr)

    def test_value_that_is_not_a_key_is_refused_and_never_printed(self):
        self.key_file.write_text('sec"ret\\value\n')
        result = self.run_helper("get")
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(result.stdout, "")
        self.assertNotIn("sec", result.stderr)
        self.assertNotIn("value", result.stderr)

    def test_commands_other_than_get_are_refused(self):
        self.key_file.write_text("AbC123xyz\n")
        for args in ([], ["store"], ["get", "extra"]):
            with self.subTest(args=args):
                result = self.run_helper(*args)
                self.assertEqual(result.returncode, 2)
                self.assertEqual(result.stdout, "")
                self.assertNotIn("AbC123xyz", result.stderr)


if __name__ == "__main__":
    unittest.main()
