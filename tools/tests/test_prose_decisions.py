#!/usr/bin/env python3
"""No control decision reads prose: the checkout matches the allowlist and every audited case stays detected."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest


path = Path(__file__).resolve().parents[1] / "prose_decisions.py"
spec = importlib.util.spec_from_file_location("prose_decisions", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


AUDITED = {
    *(f"D-{n:02}" for n in [*range(1, 18), 19, 21, 22, 25]),
    *(f"C-{n:02}" for n in range(1, 8)),
    "G-11", "G-14", "G-15", "P-03", "P-04", "A-01", "A-02", "A-03", "A-04", "A-06", "codex-mcp-denial",
}


def checkout():
    """The checkout this test runs from: Bazel's runfiles link the Makefile back to it."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        makefile = Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main") / "Makefile"
        return makefile.resolve().parent
    return Path(__file__).resolve().parents[2]


def flagged(language, text):
    return [m.text for m in guard.matches_in(language, "x", text)]


class CheckoutTest(unittest.TestCase):
    def test_every_match_is_allowed_and_every_entry_still_matches(self):
        problems = guard.report(checkout())
        self.assertEqual(problems, [], "\n" + "\n".join(problems))


class PatternTest(unittest.TestCase):
    def test_an_error_text_deciding_a_branch_is_flagged(self):
        self.assertEqual(flagged("rust", 'if stderr.contains("already merged") {'), ['if stderr.contains("already merged") {'])
        self.assertTrue(flagged("ts", "if (error.message.includes('No conversation found')) return"))
        self.assertTrue(flagged("swift", 'if error.localizedDescription.contains("offline") {'))

    def test_comments_and_rust_test_code_are_left_out(self):
        source = (
            'fn live() {}\n'
            '// if stderr.contains("already merged") {}\n'
            '/* error.to_string().contains("gone away") */\n'
            '#[cfg(test)]\n'
            'mod tests {\n'
            '    fn t() { assert!(stderr.contains("already merged")); }\n'
            '}\n'
            'fn after() { let url = "https://example.com"; if stderr.contains("was merged") {} }\n'
        )
        found = guard.matches_in("rust", "x.rs", source)
        self.assertEqual([(m.line, m.pattern) for m in found], [(8, "phrase-method")])

    def test_a_typed_decision_is_not_flagged(self):
        self.assertEqual(flagged("rust", "if output.status.code() == Some(4) { return SignedOut; }"), [])
        self.assertEqual(flagged("rust", 'if v["subtype"] == "error_during_execution" {'), [])
        self.assertEqual(flagged("swift", "if case .conflict(let kind?, _, _) = error, kind == .outcomeUnknown {"), [])

    def test_every_audited_finding_is_detected(self):
        cases = guard.read_corpus(checkout() / guard.CORPUS)
        self.assertEqual({finding for _, finding, _, _ in cases}, AUDITED)
        self.assertEqual(guard.undetected(cases), [])


class AllowlistTest(unittest.TestCase):
    def match(self, text):
        return guard.Match("crates/a.rs", 3, text, "phrase-method")

    def entry(self, text):
        return guard.Entry("crates/a.rs", guard.normalized(text), "display", "shown", 1)

    def test_an_entry_matches_its_line_whatever_its_spacing(self):
        new, stale, duplicates = guard.compare([self.match('ok &&   x.contains("a b")')], [self.entry('ok && x.contains("a b")')])
        self.assertEqual((new, stale, duplicates), ([], [], []))

    def test_a_new_match_and_an_entry_that_matches_nothing_both_fail(self):
        new, stale, _ = guard.compare([self.match('y.contains("c d")')], [self.entry('x.contains("a b")')])
        self.assertEqual([m.text for m in new], ['y.contains("c d")'])
        self.assertEqual([e.fingerprint for e in stale], ['x.contains("a b")'])

    def test_an_entry_needs_a_known_category_and_a_reason(self):
        folder = tempfile.TemporaryDirectory()
        self.addCleanup(folder.cleanup)
        allowlist = Path(folder.name) / "allowlist.tsv"
        allowlist.write_text("a.rs\tx\tmaybe\twhy\na.rs\ty\tdisplay\n", encoding="utf-8")
        _, problems = guard.read_allowlist(allowlist)
        self.assertEqual(len(problems), 2)


if __name__ == "__main__":
    unittest.main()
