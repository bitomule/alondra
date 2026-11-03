#!/usr/bin/env python3
"""The license, its notices and every place that declares it agree."""
import hashlib
import importlib.util
import json
import os
import re
import unittest
from pathlib import Path

ROOT = Path(os.environ["TEST_SRCDIR"], os.environ["TEST_WORKSPACE"]) if "TEST_SRCDIR" in os.environ else Path(__file__).absolute().parents[2]
spec = importlib.util.spec_from_file_location("third_party_notices", ROOT / "tools/third_party_notices.py")
notices = importlib.util.module_from_spec(spec)
spec.loader.exec_module(notices)


class LicenseTests(unittest.TestCase):
    def test_license_is_the_unmodified_fsl_template_with_the_copyright_line(self):
        text = (ROOT / "LICENSE.md").read_text()
        self.assertTrue(text.startswith("# Functional Source License, Version 1.1, MIT Future License\n"))
        self.assertIn("\nFSL-1.1-MIT\n", text)
        self.assertIn("\nCopyright 2026 David Collado Sela\n", text)
        self.assertNotIn("${", text)

    def test_every_manifest_declares_fsl(self):
        workspace = (ROOT / "Cargo.toml").read_text().split("[workspace.package]", 1)[1].split("\n[", 1)[0]
        self.assertIn('\nlicense = "FSL-1.1-MIT"\n', workspace)
        for manifest in ("agent-host/package.json", "web/package.json"):
            self.assertEqual(json.loads((ROOT / manifest).read_text())["license"], "FSL-1.1-MIT", manifest)

    def test_notices_were_generated_from_the_current_lockfiles(self):
        self.assertEqual(notices.stale(ROOT), [])

    def test_notices_say_the_agent_sdk_is_not_under_fsl(self):
        text = (ROOT / "THIRD_PARTY_NOTICES.md").read_text()
        section = text.split("## Anthropic Agent SDK: not under FSL", 1)[1].split("\n## ", 1)[0]
        self.assertIn("not covered by Alondra's license", section)
        self.assertIn("All rights reserved", section)
        self.assertNotIn("@anthropic-ai/claude-agent-sdk 0.3", text.split("## Agent SDK host packages", 1)[1])

    def test_notices_cover_the_fonts_the_website_serves(self):
        text = (ROOT / "THIRD_PARTY_NOTICES.md").read_text()
        for font in notices.FONT_LICENSES:
            self.assertIn((ROOT / f"web/app/public/fonts/OFL-{font}.txt").read_text().strip(), text)

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
