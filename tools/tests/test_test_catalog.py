#!/usr/bin/env python3
"""The test catalog tells a test that moved from one that was lost or stopped running."""
import importlib.util
import unittest
from pathlib import Path

path = Path(__file__).resolve().parents[1] / "test_catalog.py"
spec = importlib.util.spec_from_file_location("test_catalog", path)
catalog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(catalog)

ENGINE = "//crates/engine-sdk:engine_sdk_test"
ROOT = "//crates/alondrad:codex_adoption_process_test"


def rows(*lines):
    return catalog.parse("\n".join("\t".join(line) for line in lines))


class TestCatalogTest(unittest.TestCase):
    def test_a_listing_keeps_the_tests_and_drops_the_summary(self):
        output = "sdk_owner::tests::a_node_answers: test\nbench::measure: benchmark\n\n1 test, 1 benchmark\n"
        self.assertEqual(catalog.listed(output), ["sdk_owner::tests::a_node_answers"])

    def test_a_test_in_another_target_and_module_is_the_same_test(self):
        before = rows((ENGINE, "sdk_owner::tests::codex_tests::process_adoption::a_daemon_adopts", "ignored"))
        after = rows((ROOT, "a_daemon_adopts", "manual"))
        result = catalog.compare(before, after, {})
        self.assertEqual(len(result["moved"]), 1)
        self.assertFalse(catalog.failed(result))

    def test_a_test_that_is_nowhere_is_lost_and_named(self):
        before = rows((ENGINE, "pauses::a_pause_holds", "run"), (ENGINE, "pauses::a_resume_frees", "run"))
        after = rows((ENGINE, "pauses::a_pause_holds", "run"))
        result = catalog.compare(before, after, {})
        self.assertEqual(result["lost"], [(ENGINE, "pauses::a_resume_frees")])
        self.assertTrue(catalog.failed(result))
        self.assertIn("LOST: `pauses::a_resume_frees`", catalog.report(result))

    def test_two_tests_of_one_name_need_two_tests_after(self):
        before = rows((ENGINE, "claude::it_resumes", "run"), (ENGINE, "codex::it_resumes", "run"))
        after = rows((ROOT, "it_resumes", "run"))
        self.assertEqual(len(catalog.compare(before, after, {})["lost"]), 1)

    def test_a_test_compiled_into_two_targets_is_kept_while_one_still_has_it(self):
        other = "//crates/engine-sdk:engine_sdk_process_test"
        before = rows((ENGINE, "pauses::a_pause_holds", "run"), (other, "pauses::a_pause_holds", "manual"))
        after = rows((ENGINE, "pauses::a_pause_holds", "run"))
        result = catalog.compare(before, after, {})
        self.assertEqual((len(result["kept"]), result["lost"], result["stopped"]), (2, [], []))
        self.assertFalse(catalog.failed(result))
        gone = catalog.compare(before, rows((other, "pauses::a_pause_holds", "manual")), {})
        self.assertEqual(gone["stopped"], ["a_pause_holds"])

    def test_the_moves_explain_a_test_replaced_by_another(self):
        before = rows((ENGINE, "sdk_owner::tests::a_send_is_sent_once", "run"))
        after = rows(("//crates/provider-claude:host_contract_double_test", "sends::a_send_reaches_the_host_once", "run"))
        moves = catalog.moves_of("# what became of each test\na_send_is_sent_once\tsends::a_send_reaches_the_host_once\tnow a contract case\n")
        result = catalog.compare(before, after, moves)
        self.assertEqual(result["lost"], [])
        self.assertFalse(catalog.failed(result))
        self.assertIn("went to sends::a_send_reaches_the_host_once: now a contract case", catalog.report(result))

    def test_a_move_nothing_needed_fails(self):
        same = rows((ENGINE, "pauses::a_pause_holds", "run"))
        result = catalog.compare(same, same, {"a_pause_holds": ("elsewhere", "no reason")})
        self.assertEqual(result["unused"], ["a_pause_holds"])
        self.assertTrue(catalog.failed(result))

    def test_a_test_that_ran_and_no_longer_does_fails_until_the_moves_say_why(self):
        before = rows((ENGINE, "pauses::a_pause_holds", "run"))
        for state, target in (("ignored", ENGINE), ("manual", ROOT)):
            after = rows((target, "pauses::a_pause_holds", state))
            result = catalog.compare(before, after, {})
            self.assertEqual(result["stopped"], ["a_pause_holds"])
            self.assertTrue(catalog.failed(result))
            explained = catalog.compare(before, after, {"a_pause_holds": ("the release's tests", "it starts the real daemon")})
            self.assertFalse(catalog.failed(explained))

    def test_a_test_that_never_ran_may_move_to_a_manual_target(self):
        before = rows((ENGINE, "process_adoption::a_daemon_adopts", "ignored"))
        after = rows((ROOT, "a_daemon_adopts", "manual"))
        self.assertEqual(catalog.compare(before, after, {})["stopped"], [])

    def test_a_malformed_line_of_the_moves_is_refused_with_its_number(self):
        with self.assertRaisesRegex(ValueError, "line 2"):
            catalog.moves_of("a_test\twhere\twhy\nonly_a_name\n")

    def test_a_state_is_ignored_before_manual(self):
        self.assertEqual(catalog.state_of(True, True), "ignored")
        self.assertEqual(catalog.state_of(False, True), "manual")
        self.assertEqual(catalog.state_of(False, False), "run")


if __name__ == "__main__":
    unittest.main()
