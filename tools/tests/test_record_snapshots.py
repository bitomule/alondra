#!/usr/bin/env python3
"""Incomplete or stale OS migration captures must never overwrite committed references."""
import importlib.util
import os
from pathlib import Path
import sys
import subprocess
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]
sys.path.insert(0, str(ROOT / "tools"))
spec = importlib.util.spec_from_file_location("record_snapshots", ROOT / "tools/record_snapshots.py")
record = importlib.util.module_from_spec(spec)
spec.loader.exec_module(record)


class RecordingTests(unittest.TestCase):
    def test_complete_current_generation_is_copied_and_stale_partial_output_is_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            logs = root / "logs"
            relative = Path(next(iter(record.TARGETS.values()))) / "Suite/test.png"
            original = root / relative
            original.parent.mkdir(parents=True)
            original.write_bytes(b"original")
            target = next(iter(record.TARGETS))
            stale = logs / target / "test.outputs/old" / relative
            stale.parent.mkdir(parents=True)
            stale.write_bytes(b"stale")
            with self.assertRaises(ValueError):
                record.collect(root, logs, "current")
            self.assertEqual(original.read_bytes(), b"original")
            current = logs / target / "test.outputs/current" / relative
            current.parent.mkdir(parents=True)
            current.write_bytes(b"current")
            captured = record.collect(root, logs, "current")
            record.copy_references(root, captured)
            self.assertEqual(original.read_bytes(), b"current")
            duplicate = logs / "another-target/test.outputs/current" / relative
            duplicate.parent.mkdir(parents=True)
            duplicate.write_bytes(b"different")
            with patch.dict(record.TARGETS, {"another-target": str(relative.parent.parent)}):
                with self.assertRaisesRegex(ValueError, "Duplicate reference"):
                    record.collect(root, logs, "current")
            self.assertEqual(original.read_bytes(), b"current")
            current.with_name("unexpected.png").write_bytes(b"unexpected")
            with self.assertRaises(ValueError):
                record.collect(root, logs, "current")
            self.assertEqual(original.read_bytes(), b"current")

    def test_ci_refuses_recording_before_starting_a_simulator_or_bazel(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "true"}), patch.object(record, "probe") as probe:
            with self.assertRaises(RuntimeError):
                record.main()
            probe.assert_not_called()

    def test_bazel_uses_the_same_pinned_xcode_as_the_probe_and_build_failure_copies_nothing(self):
        with patch.dict(os.environ, {"GITHUB_ACTIONS": "false"}), \
             patch.object(record, "probe"), patch.object(record, "config", return_value={"xcode_version": "canonical"}), \
             patch.object(record.subprocess, "run", return_value=subprocess.CompletedProcess([], 1)) as run, \
             patch.object(record, "collect") as collect:
            self.assertEqual(record.main(), 1)
            self.assertIn("--xcode_version=canonical", run.call_args.args[0])
            collect.assert_not_called()


if __name__ == "__main__":
    unittest.main()
