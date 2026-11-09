#!/usr/bin/env python3
"""External PR filtering, MUSTS target coverage, and the unprivileged workflow contract."""
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import unittest
from unittest.mock import patch

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]
sys.path.insert(0, str(ROOT / "tools"))
spec = importlib.util.spec_from_file_location("ci", ROOT / "tools/ci.py")
ci = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ci)

MANIFEST = '''version: 1
checks:
  crate-domain:
    uses: bazel/test
    paths:
      - "crates/domain/**"
    with:
      targets:
        - //crates/domain/...
  crate-alondrad:
    uses: bazel/test
    paths:
      - "crates/domain/**"
      - "crates/alondrad/**"
    with:
      targets:
        - //crates/alondrad/...
'''


def event(author="owner", association="OWNER", fork=False):
    return {"repository": {"owner": {"login": "owner"}}, "pull_request": {
        "user": {"login": author}, "author_association": association,
        "head": {"repo": {"fork": fork}},
    }}


class SelectionTests(unittest.TestCase):
    def test_owner_is_excluded_including_an_owner_fork_and_reruns_by_another_actor(self):
        for fork in (False, True):
            value = event(fork=fork)
            value["sender"] = {"login": "another-maintainer"}
            self.assertFalse(ci.external_pr(value))

    def test_external_first_time_and_returning_contributors_are_included(self):
        for association in ("NONE", "FIRST_TIMER", "FIRST_TIME_CONTRIBUTOR", "CONTRIBUTOR", "COLLABORATOR"):
            for fork in (False, True):
                self.assertTrue(ci.external_pr(event("contributor", association, fork)))

    def test_manual_dispatch_uses_the_default_branch_and_never_changes_pr_author_filtering(self):
        value = event()
        value["repository"]["default_branch"] = "main"
        with patch.object(ci, "command", return_value=b"a" * 40 + b"\n") as command:
            self.assertEqual(ci.selection_base("workflow_dispatch", value), "a" * 40)
            command.assert_called_once_with("git", "rev-parse", "--verify", "refs/remotes/origin/main^{commit}")
        for name in ("pull_request", "push", "pull_request_target"):
            with self.subTest(event=name), self.assertRaises(ValueError):
                ci.selection_base(name, value)
        value["ref"] = "refs/heads/unrelated"
        with self.assertRaises(ValueError):
            ci.selection_base("push", value)
        value["ref"] = "refs/heads/worktree-t-github-actions-ci-check"
        with patch.object(ci, "command", return_value=b"a" * 40 + b"\n"):
            self.assertEqual(ci.selection_base("push", value), "a" * 40)
        external = event("contributor", "NONE", True)
        external["pull_request"]["base"] = {"sha": "b" * 40}
        self.assertEqual(ci.selection_base("pull_request", external), "b" * 40)
        checks = ci.manifest(MANIFEST)
        self.assertEqual(len(ci.plan(checks, checks, set(), force_all=True)["include"]), 2)

    def test_rename_and_delete_include_old_paths_even_when_names_contain_newlines(self):
        self.assertEqual(ci.changed_paths(b"R100\0crates/domain/old.rs\0docs/new.rs\0D\0file\nname\0"),
                         {"crates/domain/old.rs", "docs/new.rs", "file\nname"})
        with self.assertRaises(ValueError):
            ci.changed_paths(b"R100\0old.rs\0")

    def test_docs_rust_apple_and_mixed_changes_select_the_union_without_running_unrelated_checks(self):
        checks = ci.manifest(MANIFEST)
        self.assertEqual(ci.plan(checks, checks, {"docs/guide.md"}), {"include": []})
        jobs = ci.plan(checks, checks, {"crates/domain/src/lib.rs"})["include"]
        self.assertEqual([(j["check"], j["runner"]) for j in jobs],
                         [("crate-domain", "ubuntu-24.04"), ("crate-alondrad", "xcode-27")])
        jobs = ci.plan(checks, checks, {"crates/alondrad/src/lib.rs"})["include"]
        self.assertEqual([j["check"] for j in jobs], ["crate-alondrad"])

    def test_old_scopes_cannot_be_weakened_by_a_pr_and_bootstrap_changes_select_every_check(self):
        old = ci.manifest(MANIFEST)
        new = ci.manifest(MANIFEST.replace('"crates/domain/**"', '"new/**"'))
        self.assertEqual(len(ci.plan(new, old, {"crates/domain/src/lib.rs"})["include"]), 2)
        for path in ci.BOOTSTRAP:
            self.assertEqual(len(ci.plan(new, old, {path})["include"]), 2)

    def test_manifest_refuses_unsupported_or_missing_checks_and_target_flags(self):
        invalid = [MANIFEST.replace("bazel/test", "agent"),
                   MANIFEST.replace('"crates/domain/**"', "[crates/domain/**]"),
                   MANIFEST.replace('"crates/domain/**"', '"crates/domain/#fixture"'),
                   MANIFEST.replace("crate-domain:", "future-check:"),
                   MANIFEST.replace("//crates/domain/...", "--test_filter=skip"),
                   MANIFEST + "  crate-domain:\n",
                   "version: 1\nchecks:\n"]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(ValueError):
                ci.manifest(value)

    def test_every_current_manifest_target_has_exactly_one_runner(self):
        checks = ci.manifest((ROOT / "MUSTS.yml").read_text())
        jobs = ci.plan(checks, checks, {"MUSTS.yml"})["include"]
        expected = {t for spec in checks.values() for t in spec["targets"]}
        self.assertEqual({t for job in jobs for t in job["targets"]}, expected)
        for runner in {job["runner"] for job in jobs}:
            targets = [t for job in jobs if job["runner"] == runner for t in job["targets"]]
            self.assertEqual(len(targets), len(set(targets)))

    def test_bazel_failure_is_reported_without_retry_or_personal_rc_files(self):
        args = type("Args", (), {"targets": '["//crates/domain/..."]'})()
        with patch.object(ci.subprocess, "run", return_value=subprocess.CompletedProcess([], 1)) as run:
            with patch.dict(os.environ, {}, clear=True):
                self.assertEqual(ci.run(args), 1)
            run.assert_called_once_with(["bazelisk", "--nosystem_rc", "--nohome_rc", "test", "--config=ci", "--", "//crates/domain/..."])
        args.targets = '["--test_filter=skip"]'
        with self.assertRaises(ValueError):
            ci.run(args)

    def test_library_only_target_sets_are_built_but_compile_failures_stay_red(self):
        args = type("Args", (), {"targets": '["//crates/provider-contract/..."]'})()
        for build_status in (0, 1):
            results = [subprocess.CompletedProcess([], 4), subprocess.CompletedProcess([], build_status)]
            with self.subTest(build_status=build_status), patch.object(ci.subprocess, "run", side_effect=results) as run:
                with patch.dict(os.environ, {}, clear=True):
                    self.assertEqual(ci.run(args), build_status)
                self.assertEqual([c.args[0][3] for c in run.call_args_list], ["test", "build"])


class WorkflowTests(unittest.TestCase):
    def test_only_external_prs_or_explicit_manual_runs_can_reserve_a_runner(self):
        workflow = json.loads((ROOT / ".github/workflows/ci.yml").read_text())
        self.assertEqual(workflow["on"], {"pull_request": {"branches": ["main"]}, "workflow_dispatch": {},
                                          "push": {"branches": ["worktree-t-github-actions-ci-check"]}})
        self.assertEqual(set(workflow["jobs"]), {"select", "validate"})
        select = workflow["jobs"]["select"]
        self.assertEqual(select["if"], "${{ github.event_name == 'workflow_dispatch' || (github.event_name == 'push' && github.ref == 'refs/heads/worktree-t-github-actions-ci-check') || (github.event_name == 'pull_request' && github.event.pull_request.user.login != github.repository_owner && (github.event.pull_request.author_association != 'OWNER' || github.event.pull_request.head.repo.fork)) }}")
        self.assertEqual(workflow["concurrency"]["group"],
                         "external-ci-${{ github.event_name }}-${{ github.event.pull_request.number || github.ref }}")
        validate = workflow["jobs"]["validate"]
        self.assertEqual(validate["needs"], "select")
        self.assertEqual(validate["if"], "${{ needs.select.outputs.has_checks == 'true' }}")
        self.assertEqual(validate["env"]["DEVELOPER_DIR"],
                         "${{ matrix.runner == 'xcode-27' && '/Applications/Xcode_27.1.app/Contents/Developer' || '' }}")

    def test_workflow_uses_read_only_permissions_pinned_actions_and_no_secrets_or_publication(self):
        text = (ROOT / ".github/workflows/ci.yml").read_text()
        workflow = json.loads(text)
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertNotIn("secrets.", text)
        self.assertNotIn("github.token", text)
        self.assertNotIn("pull_request_target", text)
        self.assertEqual(workflow["jobs"]["select"]["runs-on"], "ubuntu-24.04")
        for job in workflow["jobs"].values():
            self.assertNotIn("permissions", job)
            self.assertIn("timeout-minutes", job)
            for step in job["steps"]:
                if "uses" in step:
                    self.assertRegex(step["uses"], r"^actions/checkout@[a-f0-9]{40}$")
                    self.assertIs(step["with"]["persist-credentials"], False)
                else:
                    self.assertIn(step["run"], {
                        "python3 tools/ci.py plan", "python3 tools/ci.py run --targets \"$CI_TARGETS\"",
                        "python3 tools/simulator.py",
                        "xcodebuild -version\nxcrun simctl list runtimes -j\nxcrun simctl list devicetypes -j",
                    })

    def test_ci_config_does_not_enable_uploads_or_buildbuddy(self):
        rc = (ROOT / ".bazelrc").read_text()
        ci_flags = [line for line in rc.splitlines() if line.startswith(("common:ci ", "build:ci ", "test:ci "))]
        self.assertIn("common:ci --remote_cache=", ci_flags)
        self.assertIn("common:ci --noremote_upload_local_results", ci_flags)
        self.assertIn("common:ci --bes_backend=", ci_flags)
        self.assertFalse(any("--config=" in line or "buildbuddy" in line for line in ci_flags))


if __name__ == "__main__":
    unittest.main()
