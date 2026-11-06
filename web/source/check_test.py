#!/usr/bin/env python3
"""Exercise the source gate with disposable Git commits, never installed services."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch

SCRIPT = Path(__file__).resolve().with_name("check.py")
spec = importlib.util.spec_from_file_location("web_source", SCRIPT)
web = importlib.util.module_from_spec(spec)
spec.loader.exec_module(web)
MANIFEST = Path(__file__).resolve().with_name("inputs.json")


class SourceTests(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        self.root = Path(self.tmp.name)
        self.env = {**os.environ, "HOME": str(self.root), "GIT_CONFIG_NOSYSTEM": "1",
                    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_AUTHOR_NAME": "Fixture",
                    "GIT_AUTHOR_EMAIL": "fixture@example.invalid", "GIT_COMMITTER_NAME": "Fixture",
                    "GIT_COMMITTER_EMAIL": "fixture@example.invalid"}
        environment = patch.dict(os.environ, self.env)
        environment.start()
        self.addCleanup(environment.stop)
        self.source = self.root / "source"
        self.source.mkdir()
        self.run_git(self.source, "init", "-q")
        self.roots = json.loads(MANIFEST.read_text())["paths"]
        for path in self.roots:
            name = path + "/site.css" if path == "web/app" else path
            self.write(self.source, name, "fixture\n")
        self.write(self.source, web.MANIFEST, MANIFEST.read_text())
        self.save(self.source)
        self.peer = self.root / "peer"
        self.run_git(self.root, "clone", "-q", str(self.source), str(self.peer))

    def run_git(self, checkout, *args):
        return subprocess.run(["git", "-C", str(checkout), *args], env=self.env,
                              check=True, capture_output=True).stdout.decode().strip()

    def write(self, checkout, name, contents):
        path = checkout / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(contents)

    def save(self, checkout):
        self.run_git(checkout, "add", ".")
        self.run_git(checkout, "commit", "-qm", "Fixture")
        return self.run_git(checkout, "rev-parse", "HEAD")

    def gate(self, mode="mirror", source_ref="HEAD", peer_ref="HEAD"):
        return web.check(self.source, web.commit(self.source, source_ref),
                         self.peer, web.commit(self.peer, peer_ref), mode)

    def test_identical_inputs_and_operational_harness_do_not_diverge(self):
        self.write(self.peer, "web/system/run.py", "operational fixture\n")
        self.save(self.peer)
        self.assertEqual(self.gate(), [])

    def test_reports_content_missing_extra_and_mode_changes(self):
        self.write(self.peer, "web/app/site.css", "broken menu\n")
        (self.peer / "web/tokens.css").unlink()
        self.write(self.peer, "web/app/extra.ts", "duplicate\n")
        (self.peer / "web/vite.config.ts").chmod(0o755)
        self.save(self.peer)
        self.assertEqual(self.gate(), ["extra: web/app/extra.ts", "changed: web/app/site.css",
                                      "missing: web/tokens.css", "changed: web/vite.config.ts"])

    def test_only_committed_bytes_are_compared_and_refs_are_pinned(self):
        old = self.run_git(self.peer, "rev-parse", "HEAD")
        self.write(self.peer, "web/app/site.css", "uncommitted\n")
        self.assertEqual(self.gate(), [])
        self.save(self.peer)
        self.assertEqual(self.gate(), ["changed: web/app/site.css"])
        self.assertEqual(self.gate(peer_ref=old), [])
        self.write(self.source, web.MANIFEST, '{"paths": []}')
        self.assertEqual(self.gate(peer_ref=old), [])

    def test_removal_requires_explicit_absent_mode_and_reintroduction_fails(self):
        shutil.rmtree(self.peer / "web")
        self.write(self.peer, "web/system/run.py", "operational fixture\n")
        self.save(self.peer)
        self.assertTrue(all(item.startswith("missing:") for item in self.gate()))
        self.assertEqual(self.gate("absent"), [])
        self.write(self.peer, "web/app/other.tsx", "duplicate\n")
        self.save(self.peer)
        self.assertEqual(self.gate("absent"), ["duplicate: web/app/other.tsx"])

    def test_invalid_missing_and_linked_source_inputs_fail_closed(self):
        (self.source / "web/tokens.css").unlink()
        self.save(self.source)
        with self.assertRaisesRegex(ValueError, "missing source input"):
            self.gate()
        (self.source / "web/tokens.css").symlink_to("package.json")
        self.save(self.source)
        with self.assertRaisesRegex(ValueError, "regular file"):
            self.gate()

    def test_manifest_rejects_unsafe_empty_duplicate_and_overlapping_roots(self):
        for roots in ([], ["../web"], ["web/../secret"], ["web/app/"], ["web/a b"],
                      ["web/app", "web/app"], ["web/app", "web/app/site.css"]):
            with self.subTest(roots=roots):
                self.write(self.source, web.MANIFEST, json.dumps({"paths": roots}))
                self.save(self.source)
                with self.assertRaises(ValueError):
                    self.gate()

    def test_cli_lists_archive_inputs_and_returns_nonzero_on_divergence(self):
        def cli(*args):
            return subprocess.run(["python3", str(SCRIPT), *args], env=self.env,
                                  capture_output=True, text=True)
        result = cli("paths", "--source", str(self.source))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.splitlines(), self.roots)
        arguments = ("check", "--source", str(self.source), "--peer", str(self.peer), "--mode", "mirror")
        self.assertEqual(cli(*arguments).returncode, 0)
        self.write(self.peer, "web/app/site.css", "broken\n")
        self.save(self.peer)
        result = cli(*arguments)
        self.assertEqual(result.returncode, 1)
        self.assertIn("changed: web/app/site.css", result.stdout)
        self.assertEqual(cli(*arguments, "--peer-ref", "missing-ref").returncode, 2)


if __name__ == "__main__":
    unittest.main()
