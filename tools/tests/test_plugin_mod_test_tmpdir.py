#!/usr/bin/env python3
"""The node mod's test script removes the folder it makes for itself, and leaves one it was given."""
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

script = Path(__file__).resolve().parents[1] / "plugin-mod-test.sh"


class PluginModTestFolderTests(unittest.TestCase):
    def setUp(self):
        self.owner = tempfile.TemporaryDirectory()
        self.addCleanup(self.owner.cleanup)
        root = Path(self.owner.name)
        self.system_tmp = root / "system-tmp"
        self.system_tmp.mkdir()
        self.record = root / "homes"
        plugin = root / "plugin"
        (plugin / ".claude-plugin").mkdir(parents=True)
        (plugin / ".claude-plugin" / "plugin.json").write_text("{}")
        self.manifest = plugin / ".claude-plugin" / "plugin.json"
        self.bin = root / "bin"
        self.bin.mkdir()
        self.fake_claude(0)

    def fake_claude(self, status):
        claude = self.bin / "claude"
        claude.write_text(f'#!/bin/sh\necho "$HOME" >> "{self.record}"\nexit {status}\n')
        claude.chmod(0o755)

    def run_script(self, **extra):
        env = {"PATH": f"{self.bin}:/usr/bin:/bin", "TMPDIR": str(self.system_tmp), **extra}
        result = subprocess.run(["/bin/sh", str(script), str(self.manifest)], env=env, capture_output=True, text=True)
        homes = {Path(line) for line in self.record.read_text().split()}
        self.assertEqual(len(homes), 1, "the script works in one folder")
        return result, homes.pop()

    def test_the_folder_it_makes_for_itself_is_removed(self):
        result, work = self.run_script()
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertFalse(work.exists(), f"left {work}")

    def test_the_folder_it_makes_for_itself_is_removed_when_claude_fails(self):
        self.fake_claude(3)
        result, work = self.run_script()
        self.assertEqual(result.returncode, 3)
        self.assertFalse(work.exists(), f"left {work}")

    def test_a_folder_the_caller_gave_is_left_to_the_caller(self):
        given = Path(self.owner.name) / "given"
        given.mkdir()
        result, work = self.run_script(TEST_TMPDIR=str(given))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(work, given)
        self.assertTrue((given / "plugin").is_dir())


if __name__ == "__main__":
    unittest.main()
