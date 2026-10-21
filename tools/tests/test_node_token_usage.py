#!/usr/bin/env python3
"""Steps and cache-read tokens per node, from sample Claude Code transcripts."""
import importlib.util
import io
import json
import os
from contextlib import redirect_stdout
from pathlib import Path
import tempfile
import unittest

module_path = Path(__file__).resolve().parents[1] / "node_token_usage.py"
spec = importlib.util.spec_from_file_location("node_token_usage", module_path)
usage = importlib.util.module_from_spec(spec)
spec.loader.exec_module(usage)


def bash(command):
    return {"type": "tool_use", "name": "Bash", "input": {"command": command}}


def records(message_id, at, blocks, read=100_000):
    tokens = {"input_tokens": 2, "cache_creation_input_tokens": 1_000, "cache_read_input_tokens": read,
              "output_tokens": 50}
    return [{"type": "assistant", "timestamp": at,
             "message": {"id": message_id, "usage": tokens, "content": [block]}} for block in blocks]


class NodeTokenUsageTest(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory(dir=os.environ.get("TEST_TMPDIR"))
        self.addCleanup(self.directory.cleanup)
        self.projects = Path(self.directory.name)

    def transcript(self, task, steps, subagent=None):
        node = self.projects / f"-Users-someone-Projects-alondra--claude-worktrees-{task}"
        path = node / "session.jsonl" if subagent is None else node / "session" / "subagents" / f"{subagent}.jsonl"
        path.parent.mkdir(parents=True, exist_ok=True)
        lines = [json.dumps(r) for step in steps for r in step]
        path.write_text("\n".join(lines + ['{"type": "user"}', "not json"]) + "\n", encoding="utf-8")

    def report(self, *argv):
        out = io.StringIO()
        with redirect_stdout(out):
            usage.main(["--projects", str(self.projects), "--min-steps", "1", "--json", *argv])
        return json.loads(out.getvalue())

    def test_a_message_split_over_several_records_is_one_step_and_its_usage_counts_once(self):
        self.transcript("t-split", [records("m1", "2026-10-21T10:00:00Z", [bash("grep -n x a.rs"), bash("cat b.rs")])])
        task, = self.report()["tasks"]
        self.assertEqual((task["steps"], task["tool_calls"], task["cache_read"]), (1, 2, 100_000))
        self.assertEqual(task["batched_share"], 1.0)

    def test_steps_are_one_look_several_looks_a_poll_or_other_work(self):
        self.transcript("t-kinds", [
            records("look", "2026-10-21T10:00:00Z", [bash("cd crates && grep -rn retry src | head")]),
            records("looks", "2026-10-21T10:01:00Z", [bash("sed -n 1,40p a.rs; git status --short")]),
            records("poll", "2026-10-21T10:02:00Z", [bash("until grep -q done .scratch/log; do sleep 30; done")]),
            records("output", "2026-10-21T10:03:00Z", [{"type": "tool_use", "name": "BashOutput", "input": {}}]),
            records("build", "2026-10-21T10:04:00Z", [bash("./scripts/test-all.sh")]),
            records("read", "2026-10-21T10:05:00Z", [{"type": "tool_use", "name": "Read", "input": {}}]),
        ])
        task, = self.report()["tasks"]
        self.assertEqual(task["steps"], 6)
        self.assertAlmostEqual(task["one_look_share"], 2 / 6)
        self.assertAlmostEqual(task["look_share"], 3 / 6)
        self.assertAlmostEqual(task["poll_share"], 2 / 6)
        self.assertAlmostEqual(task["batched_share"], 1 / 6)

    def test_subagent_steps_are_counted_apart_and_in_the_cohort_total(self):
        self.transcript("t-explore", [records("m1", "2026-10-21T10:00:00Z", [bash("ls")], read=300_000)])
        self.transcript("t-explore", [records("s1", "2026-10-21T10:00:30Z", [bash("grep -rn x .")], read=20_000),
                                      records("s2", "2026-10-21T10:00:40Z", [bash("cat y")], read=30_000)],
                        subagent="agent-1")
        result = self.report()
        task, = result["tasks"]
        self.assertEqual((task["steps"], task["cache_read"]), (1, 300_000))
        self.assertEqual((task["subagent_steps"], task["subagent_cache_read"]), (2, 50_000))
        self.assertEqual(result["cohorts"]["all"]["cache_read_with_subagents"], 350_000)

    def test_split_compares_tasks_started_before_and_after_and_the_window_keeps_only_its_steps(self):
        self.transcript("t-old", [records("a", "2026-10-20T10:00:00Z", [bash("ls")]),
                                  records("b", "2026-10-23T10:00:00Z", [bash("ls")])])
        self.transcript("t-new", [records("c", "2026-10-22T12:00:00Z", [bash("ls")])])
        cohorts = self.report("--split", "2026-10-22T00:00", "--until", "2026-10-23")["cohorts"]
        self.assertEqual((cohorts["before"]["tasks"], cohorts["before"]["steps"]), (1, 1))
        self.assertEqual((cohorts["after"]["tasks"], cohorts["after"]["steps"]), (1, 1))


if __name__ == "__main__":
    unittest.main()
