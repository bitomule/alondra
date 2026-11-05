#!/usr/bin/env python3
"""MUSTS.yml scope checks: glob semantics, manifest parsing, proposed paths and the missing-path failure."""
from contextlib import redirect_stdout
import importlib.util
import io
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location("musts_scopes", Path(__file__).resolve().parents[1] / "musts_scopes.py")
scopes = importlib.util.module_from_spec(spec)
spec.loader.exec_module(scopes)

MANIFEST = """version: 1
checks:
  relay:
    uses: bazel/test
    paths:
      - "crates/relay/**"
      - "fixtures/push/*.json"   # trailing remark
      - "Cargo.lock"
    with:
      targets:
        - //crates/relay/...
  review:
    uses: agent
    paths:
      - "docs/**"
"""

TRACKED = [
    "Cargo.lock",
    "README.md",
    "crates/relay/README.md",
    "crates/relay/src/lib.rs",
    "crates/store/src/lib.rs",
    "crates/store/README.md",
    "docs/images/shot.png",
    "fixtures/push/a.json",
    "fixtures/push/b.json",
    "fixtures/push/notes.md",
    "fixtures/views/cards/c.json",
    "fixtures/views/d.json",
]


class MustsScopesTests(unittest.TestCase):
    def test_star_stops_at_slash_and_double_star_descends(self):
        self.assertTrue(scopes.matches(["fixtures/push/*.json"], "fixtures/push/a.json"))
        self.assertFalse(scopes.matches(["fixtures/push/*.json"], "fixtures/push/deep/a.json"))
        self.assertTrue(scopes.matches(["crates/**"], "crates/relay/src/lib.rs"))
        self.assertTrue(scopes.matches(["**/*.bazel"], "BUILD.bazel"))
        self.assertTrue(scopes.matches(["**/*.bazel"], "a/b/BUILD.bazel"))

    def test_globs_ignore_case(self):
        self.assertTrue(scopes.matches(["License.md"], "LICENSE.md"))

    def test_only_bazel_test_checks_are_read_with_paths_and_targets(self):
        checks = scopes.parse_checks(MANIFEST)
        self.assertEqual(list(checks), ["relay"])
        self.assertEqual(checks["relay"]["paths"], ["crates/relay/**", "fixtures/push/*.json", "Cargo.lock"])
        self.assertEqual(checks["relay"]["targets"], ["//crates/relay/..."])

    def test_label_becomes_a_repository_path(self):
        self.assertEqual(scopes.label_path("//crates/relay:src/lib.rs"), "crates/relay/src/lib.rs")
        self.assertEqual(scopes.label_path("//:Makefile"), "Makefile")

    def test_proposal_takes_a_whole_crate_but_leaves_out_prose_beside_other_files(self):
        read = {"crates/relay/src/lib.rs", "fixtures/push/a.json", "fixtures/push/b.json", "fixtures/views/d.json", "Cargo.lock"}
        self.assertEqual(
            scopes.propose(read, TRACKED),
            ["crates/relay/**", "Cargo.lock", "fixtures/push/*.json", "fixtures/views/d.json"],
        )

    def test_proposal_does_not_cover_a_directory_with_unread_files(self):
        self.assertNotIn("fixtures/push/**", scopes.propose({"fixtures/push/a.json"}, TRACKED))

    def run_check(self, manifest, read):
        out = io.StringIO()
        with patch.object(scopes, "closure", return_value=read), redirect_stdout(out):
            failed = scopes.check(manifest, TRACKED, set())
        return failed, out.getvalue()

    def test_a_docs_only_change_reopens_nothing(self):
        checks = scopes.parse_checks(MANIFEST)
        for doc in ("README.md", "docs/images/shot.png", "fixtures/push/notes.md", "crates/store/README.md"):
            self.assertFalse(scopes.matches(checks["relay"]["paths"], doc), doc)

    def test_check_passes_when_every_read_file_is_in_paths(self):
        failed, _ = self.run_check(MANIFEST, {"crates/relay/src/lib.rs", "fixtures/push/a.json", "Cargo.lock"})
        self.assertFalse(failed)

    def test_check_fails_naming_a_file_the_targets_read_that_no_path_matches(self):
        failed, output = self.run_check(MANIFEST, {"crates/relay/src/lib.rs", "fixtures/views/d.json"})
        self.assertTrue(failed)
        self.assertIn("missing  fixtures/views/d.json", output)

    def test_check_fails_once_a_path_is_taken_out(self):
        read = {"crates/relay/src/lib.rs", "Cargo.lock"}
        self.assertFalse(self.run_check(MANIFEST, read)[0])
        self.assertTrue(self.run_check(MANIFEST.replace('      - "Cargo.lock"\n', ""), read)[0])


if __name__ == "__main__":
    unittest.main()
