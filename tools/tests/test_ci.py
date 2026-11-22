#!/usr/bin/env python3
"""The pull request workflow certifies local musts validation and stays unprivileged."""
import json
import os
from pathlib import Path
import re
import unittest

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]


def workflow_text():
    return (ROOT / ".github/workflows/ci.yml").read_text()


def ci_job():
    return json.loads(workflow_text())["jobs"]["CI"]


class WorkflowContract(unittest.TestCase):
    def test_one_stable_job_named_ci_is_the_only_job(self):
        workflow = json.loads(workflow_text())
        self.assertEqual(set(workflow["jobs"]), {"CI"})
        self.assertEqual(workflow["jobs"]["CI"]["name"], "CI")

    def test_only_pull_requests_or_explicit_manual_runs_can_reserve_a_runner(self):
        workflow = json.loads(workflow_text())
        self.assertEqual(workflow["on"], {"pull_request": {"branches": ["main"]}, "workflow_dispatch": {}})

    def test_the_job_ends_by_running_musts_validate_on_the_checked_out_tree(self):
        steps = ci_job()["steps"]
        self.assertEqual(steps[-1]["run"], "musts validate")
        self.assertEqual(steps[0]["uses"].split("@")[0], "actions/checkout")

    def test_musts_comes_from_a_published_binary_pinned_by_version_and_hash(self):
        job = ci_job()
        self.assertRegex(job["env"]["MUSTS_VERSION"], r"^\d+\.\d+\.\d+$")
        self.assertRegex(job["env"]["MUSTS_SHA256"], r"^[a-f0-9]{64}$")
        install = next(s for s in job["steps"] if s.get("name") == "Install the published musts binary")["run"]
        self.assertIn("gh release download", install)
        self.assertIn("sha256sum --check", install)
        for compile_step in ("cargo ", "brew ", "rustup"):
            self.assertNotIn(compile_step, workflow_text())

    def test_workflow_uses_read_only_permissions_pinned_actions_and_no_secrets_or_publication(self):
        text = workflow_text()
        workflow = json.loads(text)
        job = workflow["jobs"]["CI"]
        self.assertEqual(workflow["permissions"], {"contents": "read"})
        self.assertNotIn("secrets.", text)
        self.assertNotIn("pull_request_target", text)
        self.assertNotIn("permissions", job)
        self.assertEqual(job["runs-on"], "ubuntu-24.04")
        self.assertIn("timeout-minutes", job)
        for step in job["steps"]:
            if "uses" in step:
                self.assertRegex(step["uses"], r"^actions/checkout@[a-f0-9]{40}$")
                self.assertIs(step["with"]["persist-credentials"], False)

    def test_ci_config_does_not_enable_uploads_or_buildbuddy(self):
        rc = (ROOT / ".bazelrc").read_text()
        ci_flags = [line for line in rc.splitlines() if line.startswith(("common:ci ", "build:ci ", "test:ci "))]
        self.assertIn("common:ci --remote_cache=", ci_flags)
        self.assertIn("common:ci --noremote_upload_local_results", ci_flags)
        self.assertIn("common:ci --bes_backend=", ci_flags)
        self.assertFalse(any("--config=" in line or "buildbuddy" in line for line in ci_flags))


if __name__ == "__main__":
    unittest.main()
