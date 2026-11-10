#!/usr/bin/env python3
"""The checkout carries no personal or private data, and the guard catches each kind of it."""
import importlib.util
import os
from pathlib import Path
import subprocess
import tempfile
import unittest

path = Path(__file__).resolve().parents[1] / "public_data_guard.py"
spec = importlib.util.spec_from_file_location("public_data_guard", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    """The checkout this test runs from: Bazel's runfiles link the Makefile back to it."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        makefile = Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main") / "Makefile"
        return makefile.resolve().parent
    return Path(__file__).resolve().parents[2]


class CheckoutTest(unittest.TestCase):
    def test_no_tracked_file_carries_personal_or_private_data(self):
        problems = guard.report(checkout())
        self.assertEqual(problems, [], "\n" + "\n".join(problems))


class GuardTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        (self.root / "tools").mkdir()
        self.allow("")
        saved = os.environ.pop(guard.PRIVATE_LITERALS_ENV, None)
        if saved is not None:
            self.addCleanup(os.environ.__setitem__, guard.PRIVATE_LITERALS_ENV, saved)
        self.addCleanup(os.environ.pop, guard.PRIVATE_LITERALS_ENV, None)

    def literals(self, *names):
        outside = tempfile.TemporaryDirectory()
        self.addCleanup(outside.cleanup)
        file = Path(outside.name, "literals.txt")
        file.write_text("# private names\n" + "\n".join(names) + "\n")
        os.environ[guard.PRIVATE_LITERALS_ENV] = str(file)

    def allow(self, text):
        (self.root / guard.ALLOWLIST).write_text(text)

    def problems(self, content, name="notes.txt"):
        (self.root / name).write_text(content)
        return guard.report(self.root)

    def rules(self, content):
        return [problem.split(": ")[1] for problem in self.problems(content)]

    def test_a_personal_email_fails(self):
        self.assertEqual(self.rules("write to maria.garcia@gmail.com\n"), ["personal-email"])

    def test_reserved_and_public_contact_addresses_pass(self):
        text = "ana@example.com ana@acme.com hello@getalondra.com 1+x@users.noreply.github.com git@github.com:o/r\n"
        self.assertEqual(self.problems(text), [])

    def test_a_real_home_directory_fails_and_a_generic_one_passes(self):
        self.assertEqual(self.rules("cwd /Users/mariagarcia/Projects\n"), ["home-path"])
        self.assertEqual(self.problems("cwd /Users/ana/Projects and /Users/Shared\n"), [])

    def test_a_public_address_fails_and_documentation_private_and_resolver_addresses_pass(self):
        self.assertEqual(self.rules("peer 81.40.12.7 and 2a0c:5a80::7\n"), ["public-ip", "public-ip"])
        self.assertEqual(self.problems("203.0.113.7 2001:db8::7 192.168.1.20 10.0.0.1 127.0.0.1 8.8.8.8 ::1\n"), [])

    def test_an_address_spelled_as_octets_fails_like_a_dotted_one(self):
        self.assertEqual(self.rules("const HOME: Ipv4Addr = Ipv4Addr::new(81, 40, 12, 7);\n"), ["public-ip"])
        self.assertEqual(self.problems("const HOME: Ipv4Addr = Ipv4Addr::new(203, 0, 113, 7);\n"), [])

    def test_a_device_identifier_fails_next_to_its_label_and_a_synthetic_one_passes(self):
        self.assertEqual(self.rules("udid: 3F2A9C1E-7B44-4D0A-9E55-1C8D2B6A4F90\n"), ["device-id"])
        self.assertEqual(self.problems("udid: 00000000-0000-0000-0000-000000000001\n"), [])
        self.assertEqual(self.problems("session 3F2A9C1E-7B44-4D0A-9E55-1C8D2B6A4F90\n"), [])

    def test_a_credential_fails_and_a_placeholder_passes(self):
        self.assertEqual(self.rules("token ghp_" + "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7R8\n"), ["secret"])
        self.assertEqual(self.rules("-----BEGIN RSA PRIVATE KEY-----\n"), ["secret"])
        self.assertEqual(self.problems("AKIAIOSFODNN7EXAMPLE sk-ant-public-loopback-fixture-only\n"), [])

    def test_a_tracker_id_fails(self):
        self.assertEqual(self.rules("fixes BIT-123\n"), ["tracker-id"])

    def test_literals_from_a_file_outside_the_checkout_fail_without_printing_them(self):
        self.literals("examplename", "example-private-repo")
        problems = self.problems("host.ExampleName.dev and acme/example-private-repo#6\n")
        self.assertEqual([p.split(": ")[1] for p in problems], ["private-literal", "private-literal"])
        printed = "".join(problems).lower()
        self.assertNotIn("examplename", printed)
        self.assertNotIn("example-private-repo", printed)

    def test_without_the_literals_file_the_generic_rules_still_run_and_nothing_hidden_fails(self):
        self.assertEqual(self.problems("the examplename release\n"), [])
        os.environ[guard.PRIVATE_LITERALS_ENV] = str(self.root / "missing.txt")
        self.assertEqual(self.problems("the examplename release\n"), [])
        self.assertEqual(self.rules("write to maria.garcia@gmail.com\n"), ["personal-email"])

    def test_a_private_literal_entry_allows_the_file_and_is_never_reported_as_unused(self):
        self.allow("notes.txt\tprivate-literal\t*\tkept for the migration\n")
        self.assertEqual(self.problems("nothing private here\n"), [])
        self.literals("examplename")
        self.assertEqual(self.problems("the examplename release\n"), [])
        self.assertEqual(self.problems("nothing private here\n"), [])

    def test_an_allowed_match_passes_and_an_unused_entry_fails(self):
        self.allow("notes.txt\tpersonal-email\tmaria.garcia@gmail.com\tthe reviewer's own test address\n")
        self.assertEqual(self.problems("maria.garcia@gmail.com\n"), [])
        self.assertEqual(len(self.problems("nothing here\n")), 1)
        self.assertIn("unused entry", guard.report(self.root)[0])

    def test_an_entry_without_a_reason_allows_nothing(self):
        self.allow("notes.txt\tpersonal-email\tmaria.garcia@gmail.com\t\n")
        self.assertEqual(self.rules("maria.garcia@gmail.com\n"), ["personal-email"])

    def test_the_guard_skips_binary_files(self):
        (self.root / "image.png").write_bytes(b"\x89PNG\xff\xfe maria.garcia@gmail.com")
        self.assertEqual(guard.report(self.root), [])


class DiffTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)
        self.env = {**os.environ, "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull}
        self.git("init", "-q")
        (self.root / "old.txt").write_text("contact maria.garcia@gmail.com\n")
        self.git("add", ".")
        self.git("-c", "user.name=t", "-c", "user.email=t@example.com", "commit", "-q", "-m", "base")

    def git(self, *args):
        subprocess.run(["git", "-C", str(self.root), *args], check=True, env=self.env, capture_output=True)

    def test_only_the_lines_a_diff_adds_are_checked(self):
        (self.root / "old.txt").write_text("contact maria.garcia@gmail.com\nfine line\n")
        (self.root / "new.txt").write_text("cwd /Users/mariagarcia\n")
        self.git("add", "-N", "new.txt")
        problems = guard.report(self.root, against="HEAD")
        self.assertEqual(problems, ["new.txt:1: home-path: /Users/mariagarcia"])


if __name__ == "__main__":
    unittest.main()
