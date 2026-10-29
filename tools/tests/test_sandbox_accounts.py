#!/usr/bin/env python3
"""A fresh sandbox connects Claude without replacing the Mac's other account settings."""
import importlib.util
import io
import json
from contextlib import redirect_stderr
from pathlib import Path
import unittest
from unittest.mock import patch
import urllib.error


path = Path(__file__).resolve().parents[1] / "sandbox_accounts.py"
spec = importlib.util.spec_from_file_location("sandbox_accounts", path)
accounts = importlib.util.module_from_spec(spec)
spec.loader.exec_module(accounts)


class SandboxAccountsTest(unittest.TestCase):
    def connect(self, config):
        listing = {"version": "configuration-hash", "config": config}
        replies = [io.BytesIO(json.dumps(listing).encode()), io.BytesIO(b"{}"), io.BytesIO(b"{}")]
        with patch.object(accounts.urllib.request, "urlopen", side_effect=replies) as send:
            accounts.connect_claude(48211)
        requests = [call.args[0] for call in send.call_args_list]
        self.assertEqual([request.method for request in requests], ["GET", "PUT", "POST"])
        self.assertEqual([request.full_url for request in requests], [
            "http://127.0.0.1:48211/settings/accounts?capabilities=provider_accounts",
            "http://127.0.0.1:48211/settings/accounts?capabilities=provider_accounts",
            "http://127.0.0.1:48211/settings/accounts/check?capabilities=provider_accounts",
        ])
        for call in send.call_args_list:
            self.assertEqual(call.kwargs, {"timeout": 60})
        self.assertEqual(requests[1].get_header("Content-type"), "application/json")
        self.assertIsNone(requests[0].data)
        self.assertIsNone(requests[2].data)
        body = json.loads(requests[1].data)
        self.assertEqual(body["version"], "configuration-hash")
        return body["accounts"]

    def test_fresh_configuration_connects_claude(self):
        self.assertEqual(self.connect({"connected": []}), {"connected": ["claude"]})

    def test_missing_or_null_connections_connect_claude(self):
        for config in ({}, {"connected": None}):
            with self.subTest(config=config):
                self.assertEqual(self.connect(config), {"connected": ["claude"]})

    def test_preserves_providers_profiles_and_distribution(self):
        config = {"connected": ["codex"], "claude": [{"id": "work", "name": "Work"}],
                  "distribution": {"move_when_exhausted": False}}
        self.assertEqual(self.connect(config), dict(config, connected=["codex", "claude"]))

    def test_already_connected_is_not_duplicated(self):
        config = {"connected": ["claude", "codex"]}
        self.assertEqual(self.connect(config), config)

    def test_http_failures_stop_the_sequence(self):
        for failed in range(3):
            with self.subTest(failed=failed):
                error = urllib.error.HTTPError("sandbox", 409, "conflict", {}, None)
                replies = [io.BytesIO(b'{"version":"configuration-hash","config":{}}'), io.BytesIO(b"{}")][:failed]
                with patch.object(accounts.urllib.request, "urlopen", side_effect=[*replies, error]) as send:
                    with self.assertRaises(urllib.error.HTTPError):
                        accounts.connect_claude(48211)
                self.assertEqual(send.call_count, failed + 1)

    def test_timeout_reports_the_mac_and_exits_unsuccessfully(self):
        stderr = io.StringIO()
        with patch("sys.argv", [str(path), "48211"]), redirect_stderr(stderr):
            with patch.object(accounts.urllib.request, "urlopen", side_effect=TimeoutError("timed out")):
                with self.assertRaises(SystemExit) as stopped:
                    accounts.main()
        self.assertEqual(stopped.exception.code, 1)
        self.assertIn("Could not connect Claude on sandbox port 48211: timed out", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
