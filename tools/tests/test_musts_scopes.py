#!/usr/bin/env python3
"""MUSTS.yml scope checks: glob semantics, manifest parsing, proposed paths and the missing-path failure."""
from contextlib import redirect_stdout
import importlib.util
import io
import os
import re
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

    def test_check_fails_naming_a_glob_its_targets_never_read(self):
        failed, output = self.run_check(MANIFEST, {"crates/relay/src/lib.rs", "Cargo.lock"})
        self.assertTrue(failed)
        self.assertIn("dead     fixtures/push/*.json", output)

    def test_a_root_file_no_query_names_is_never_a_dead_glob(self):
        self.assertEqual(scopes.dead_globs([".bazelrc", "crates/relay/**"], {"crates/relay/src/lib.rs"}), [])

    def test_a_glob_over_the_whole_checkout_is_never_a_dead_glob(self):
        self.assertEqual(scopes.dead_globs(["**/*.bzl"], {"crates/relay/src/lib.rs"}), [])

    def test_a_crate_change_reopens_no_screen_check(self):
        checks = scopes.parse_checks(Path(os.environ["MUSTS_MANIFEST"]).read_text())
        self.assertFalse(scopes.matches(checks["apple-tests"]["paths"], "crates/core/src/usage.rs"))
        self.assertTrue(scopes.matches(checks["apple-daemon-process"]["paths"], "crates/core/src/usage.rs"))

    def test_check_fails_once_a_path_is_taken_out(self):
        read = {"crates/relay/src/lib.rs", "fixtures/push/a.json", "Cargo.lock"}
        self.assertFalse(self.run_check(MANIFEST, read)[0])
        self.assertTrue(self.run_check(MANIFEST.replace('      - "Cargo.lock"\n', ""), read)[0])


def inventory_review(text):
    """Read the block-list subset used by the inventory's agent check."""
    block = re.search(r"^  mac-inventory-review:\n(.*?)(?=^  [\w-]+:|\Z)", text, re.M | re.S)
    if block is None:
        raise ValueError("mac-inventory-review must require source review of docs/on-your-mac.md")
    block = block.group(1)
    if not re.search(r"^    uses: agent$", block, re.M):
        raise ValueError("inventory review must require agent judgment")
    paths = re.search(r"^    paths:\n(.*?)(?=^    \w|\Z)", block, re.M | re.S)
    if paths is None:
        raise ValueError("inventory review must have explicit paths")
    patterns = re.findall(r'^      - "([^"\n]+)"$', paths.group(1), re.M)
    facts = re.findall(r'^        - "([^"\n]+)"$', block, re.M)
    if not any("docs/on-your-mac.md" in fact and "Update" in fact and "source" in fact for fact in facts):
        raise ValueError("inventory review must require updating the inventory from source")
    return patterns


INVENTORY_SURFACES = (
    "Makefile",
    "deploy/brew/alondra.rb.in",
    "deploy/brew/release.py",
    "deploy/brew/package-cli.sh",
    "deploy/brew/sign-package.sh",
    "deploy/brew/release.sh",
    "launchd/alondra.plist.in",
    ".claude-plugin/marketplace.json",
    "plugin/.mcp.json",
    "plugin/hooks/hooks.json",
    "plugin/hooks/register.ts",
    "plugin/bin/alondra",
    "plugin-orchestrator/.mcp.json",
    "agent-host/session.ts",
    "agent-host/codex-session.ts",
    "agent-host/sea.mjs",
    "agent-host/toolchain.bzl",
    "agent-host/package.json",
    "agent-host/pnpm-lock.yaml",
    "agent-host/host-entitlements.plist",
    "crates/alondra/src/setup.rs",
    "crates/alondra/src/service_setup/recovery.rs",
    "crates/alondra/src/account.rs",
    "crates/alondrad/Info.plist",
    "crates/alondrad/src/iroh_endpoint.rs",
    "crates/alondrad/src/service/relay.rs",
    "crates/daemon-engine/src/sdk_owner/backend.rs",
    "crates/daemon-support/src/launch_worktree.rs",
    "crates/daemon-support/src/codex_credentials.rs",
    "crates/daemon-support/src/new_runtime_surface.rs",
    "crates/agent-runtime/src/owned_host.rs",
    "crates/provider-claude/src/adapter.rs",
    "crates/provider-codex/src/adapter.rs",
    "crates/process-group/src/lib.rs",
    "crates/store/src/lib.rs",
    "crates/core/src/launcher.rs",
    "crates/core/src/secret_destination.rs",
    "crates/core/src/config.rs",
    "crates/core/src/bonjour.rs",
    "crates/core/src/startup.rs",
    "crates/core/src/cited_file.rs",
    "crates/core/src/issues.rs",
    "crates/core/src/claude_account.rs",
    "crates/core/src/codex_account.rs",
    "apple/entitlements.bzl",
    "apple/BUILD.bazel",
    "apple/macOS/Info.plist",
    "apple/macOS/ViewsShipping/MacAppDelegate.swift",
    "apple/macOS/Workspace/MacWorkspaceOwner.swift",
    "apple/Bonjour/Sources/new_discovery.swift",
    "apple/Data/Account/Sources/Auth/KeychainRefreshTokenStorage.swift",
    "apple/Data/Pairing/Sources/PairingStore.swift",
    "apple/Data/Transport/Sources/Loopback/LoopbackIdentityStore.swift",
    "apple/Data/Iroh/Sources/new_transport.swift",
    "apple/Data/Notifications/Sources/LocalNotices/SystemLocalNoticeCenter.swift",
    "apple/Data/Attachments/Sources/new_upload.swift",
)


class InventoryReviewTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.manifest = Path(os.environ["MUSTS_MANIFEST"]).read_text()

    def test_runtime_changes_require_inventory_review(self):
        patterns = inventory_review(self.manifest)
        for path in INVENTORY_SURFACES:
            with self.subTest(path=path):
                self.assertTrue(scopes.matches(patterns, path), path)

    def test_unrelated_prose_and_other_products_do_not_require_inventory_review(self):
        patterns = inventory_review(self.manifest)
        for path in ("README.md", "INSTALL.md", "docs/architecture/overview.md", "docs/images/example.png", "web/app/routes/install.tsx"):
            with self.subTest(path=path):
                self.assertFalse(scopes.matches(patterns, path), path)

    def test_removing_the_review_gate_is_rejected(self):
        without = re.sub(r"^  mac-inventory-review:\n.*?(?=^  [\w-]+:|\Z)", "", self.manifest, flags=re.M | re.S)
        with self.assertRaisesRegex(ValueError, "must require source review"):
            inventory_review(without)

    def test_dropping_launchd_coverage_is_detected(self):
        patterns = inventory_review(self.manifest.replace('      - "launchd/**"\n', ""))
        self.assertFalse(scopes.matches(patterns, "launchd/alondra.plist.in"))


if __name__ == "__main__":
    unittest.main()
