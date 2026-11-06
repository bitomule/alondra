#!/usr/bin/env python3
"""The license, its notices and every place that declares it agree."""
import hashlib
import importlib.util
import json
import os
import re
import unittest
from pathlib import Path

ELV2_SHA256 = "48255018b41fc0e965b1115af7e6779bc218bb8a6747d561da800d5022622aa2"
ROOT = Path(os.environ["TEST_SRCDIR"], os.environ["TEST_WORKSPACE"]) if "TEST_SRCDIR" in os.environ else Path(__file__).absolute().parents[2]
spec = importlib.util.spec_from_file_location("third_party_notices", ROOT / "tools/third_party_notices.py")
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class LicenseTests(unittest.TestCase):
    def test_license_is_the_unmodified_elv2_text_with_the_copyright_line(self):
        text = (ROOT / "LICENSE.md").read_text()
        self.assertTrue(text.startswith("Copyright 2026 David Collado Sela\n\nElastic License 2.0\n\nURL: https://www.elastic.co/licensing/elastic-license\n"))
        self.assertTrue(text.rstrip().endswith("**trademark** means trademarks, service marks, and similar rights."))
        self.assertEqual(hashlib.sha256(text.split("\n\n", 1)[1].encode()).hexdigest(), ELV2_SHA256)
        for former in ("Functional Source", "FSL", "Future License"):
            self.assertNotIn(former, text)

    def test_every_manifest_declares_elv2(self):
        workspace = (ROOT / "Cargo.toml").read_text().split("[workspace.package]", 1)[1].split("\n[", 1)[0]
        self.assertIn('\nlicense = "Elastic-2.0"\n', workspace)
        self.assertEqual(json.loads((ROOT / "agent-host/package.json").read_text())["license"], "Elastic-2.0")

    def test_notices_were_generated_from_the_current_lockfiles(self):
        self.assertEqual(notices.stale(ROOT), [])

    def test_app_licenses_screen_data_matches_the_license_and_the_notices(self):
        self.assertEqual((ROOT / notices.APP_LICENSES).read_text(), notices.app_licenses_source(ROOT))

    def test_notices_say_the_agent_sdk_is_not_under_elv2(self):
        text = (ROOT / "THIRD_PARTY_NOTICES.md").read_text()
        section = text.split("## Anthropic Agent SDK: not under ELv2", 1)[1].split("\n## ", 1)[0]
        self.assertIn("not covered by Alondra's license", section)
        self.assertIn("All rights reserved", section)
        self.assertNotIn("@anthropic-ai/claude-agent-sdk 0.3", text.split("## Agent SDK host packages", 1)[1])

    def test_node_license_matches_the_pinned_node(self):
        pinned = notices.node_version(ROOT)
        self.assertEqual(pinned, re.search(r"Node\.js (\S+) \(MIT", (ROOT / "THIRD_PARTY_NOTICES.md").read_text())[1])
        self.assertIn("Node.js is licensed for use as follows", (ROOT / "deploy/brew/node/LICENSE").read_text())

    def test_a_changed_lockfile_makes_the_notices_stale(self):
        lock = (ROOT / "Cargo.lock").read_text()
        newer = re.sub(r'(name = "anyhow"\nversion = ")', r'\g<1>9', lock, count=1)
        self.assertNotEqual(
            notices.digest(notices.locked_registry_packages(lock)),
            notices.digest(notices.locked_registry_packages(newer)),
        )

    def test_a_new_workspace_version_does_not_make_the_notices_stale(self):
        lock = (ROOT / "Cargo.lock").read_text()
        bumped = re.sub(r'(name = "alondrad"\nversion = ")[^"]*', r'\g<1>99.0.0', lock, count=1)
        self.assertEqual(notices.locked_registry_packages(lock), notices.locked_registry_packages(bumped))


if __name__ == "__main__":
    unittest.main()
