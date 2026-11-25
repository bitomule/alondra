#!/usr/bin/env python3
"""Every program a test runs besides itself is declared, has a reason, and their number only goes down."""
import importlib.util
import os
import shutil
import tempfile
import unittest
from pathlib import Path

ROOT = Path(os.environ["TEST_SRCDIR"]) / os.environ["TEST_WORKSPACE"]
spec = importlib.util.spec_from_file_location("test_execs", ROOT / "tools/test_execs.py")
test_execs = importlib.util.module_from_spec(spec)
spec.loader.exec_module(test_execs)


def target(execs=None, tags=(), data=()):
    return {"kind": "rust_test", "execs": execs, "tags": list(tags), "data": list(data)}


class Repository(unittest.TestCase):
    def test_every_declared_program_is_well_formed_explained_and_under_its_ceiling(self):
        tests = test_execs.read_tests(ROOT / test_execs.RULES)
        self.assertEqual(test_execs.check(tests, ROOT), [])

    def test_only_the_release_only_tests_turn_the_guard_off(self):
        tests = test_execs.read_tests(ROOT / test_execs.RULES)
        off = sorted(label for label, test in tests.items() if test["execs"] == test_execs.E2E)
        self.assertEqual(off, ["//crates/alondrad:codex_adoption_process_test", "//crates/engine-sdk:engine_sdk_host_test"])


class Entries(unittest.TestCase):
    def problems(self, test):
        return test_execs.problems_of("//crates/a:a_test", test)

    def test_system_paths_data_executables_path_programs_and_the_temporary_folder_are_programs(self):
        test = target("/usr/bin/git $(rootpath :fake) $(rootpath //crates/b:tool) path:claude tmpdir", data=["//crates/a:fake", "//crates/b:tool"])
        self.assertEqual(self.problems(test), [])

    def test_a_path_outside_the_system_folders_is_refused(self):
        for entry in ("/opt/homebrew/bin/gh", "/usr/local/bin/claude", "git", "/usr/bin/../bin/git", "path:../gh"):
            self.assertEqual(len(self.problems(target(entry))), 1, entry)

    def test_a_rootpath_of_something_the_target_does_not_declare_is_refused(self):
        self.assertIn("names no executable of its `data`", self.problems(target("$(rootpath :other)", data=[":fake"]))[0])

    def test_a_program_named_twice_is_refused(self):
        self.assertIn("twice", self.problems(target("/usr/bin/git /usr/bin/git"))[0])

    def test_own_sandbox_stands_alone(self):
        self.assertEqual(self.problems(target("own-sandbox")), [])
        self.assertIn("only entry", self.problems(target("own-sandbox /usr/bin/git"))[0])

    def test_only_a_manual_target_tagged_e2e_turns_the_guard_off(self):
        self.assertEqual(self.problems(target("e2e", tags=["e2e", "manual"])), [])
        self.assertIn("only a target tagged e2e", self.problems(target("e2e"))[0])
        self.assertIn("`manual` too", self.problems(target("e2e", tags=["e2e"]))[0])
        self.assertIn("alone", self.problems(target(None, tags=["e2e", "manual"]))[0])


class Census(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(dir=os.environ.get("TEST_TMPDIR")))
        self.addCleanup(shutil.rmtree, self.root)
        (self.root / "tools").mkdir()
        (self.root / "docs").mkdir()
        self.tests = {"//crates/a:a_test": target("/usr/bin/true")}
        (self.root / test_execs.REASONS).write_text("/usr/bin/true\tIt measures how long a process takes to start.\n")
        rows = test_execs.declared(self.tests)
        (self.root / test_execs.DOC).write_text("# Tests\n\n" + test_execs.table(rows, test_execs.read_reasons(self.root)) + "\n")
        self.ceilings = (test_execs.EXEC_CEILING, test_execs.OWN_SANDBOX_CEILING)
        self.addCleanup(self.restore)
        test_execs.EXEC_CEILING, test_execs.OWN_SANDBOX_CEILING = 1, 0

    def restore(self):
        test_execs.EXEC_CEILING, test_execs.OWN_SANDBOX_CEILING = self.ceilings

    def test_declared_programs_at_their_ceiling_pass(self):
        self.assertEqual(test_execs.check(self.tests, self.root), [])

    def test_a_new_program_above_the_ceiling_fails(self):
        self.tests["//crates/b:b_test"] = target("/usr/bin/true")
        self.assertTrue(any("EXEC_CEILING = 1" in problem for problem in test_execs.check(self.tests, self.root)))

    def test_a_ceiling_above_the_programs_asks_to_be_lowered(self):
        test_execs.EXEC_CEILING = 2
        self.assertIn("lower EXEC_CEILING in tools/test_execs.py to 1", test_execs.check(self.tests, self.root)[0])

    def test_a_program_without_a_reason_and_a_reason_without_a_program_fail(self):
        self.tests["//crates/a:a_test"] = target("/usr/bin/id")
        found = test_execs.check(self.tests, self.root)
        self.assertTrue(any("`/usr/bin/id` has no reason" in problem for problem in found))
        self.assertTrue(any("`/usr/bin/true`, which no target runs" in problem for problem in found))

    def test_a_table_that_is_not_the_declared_one_fails(self):
        doc = self.root / test_execs.DOC
        doc.write_text(doc.read_text().replace("/usr/bin/true", "/usr/bin/false", 1))
        self.assertTrue(any("bazel run //tools:test_execs -- write" in problem for problem in test_execs.check(self.tests, self.root)))


if __name__ == "__main__":
    unittest.main()
