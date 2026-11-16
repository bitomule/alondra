#!/usr/bin/env python3
"""The daemon's production code reaches outside its process only through core's door, and the guard names each way of not doing so."""
import importlib.util
import os
from pathlib import Path
import tempfile
import unittest

path = Path(__file__).resolve().parents[1] / "outside_guard.py"
spec = importlib.util.spec_from_file_location("outside_guard", path)
guard = importlib.util.module_from_spec(spec)
spec.loader.exec_module(guard)


def checkout():
    """The checkout as this test declares it: the `tree` targets in its data, read from its runfiles."""
    runfiles = os.environ.get("TEST_SRCDIR")
    if runfiles:
        return Path(runfiles) / os.environ.get("TEST_WORKSPACE", "_main")
    return Path(__file__).resolve().parents[2]


class CheckoutTest(unittest.TestCase):
    def test_no_daemon_crate_runs_a_program_or_opens_a_connection_of_its_own(self):
        found = guard.problems(checkout())
        self.assertEqual(found, [], "\n" + "\n".join(found))


class GuardTest(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def write(self, relative, text, crate="daemon-engine"):
        target = self.root / "crates" / crate / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def test_a_process_or_a_connection_of_its_own_is_named(self):
        for line in [
            'let o = std::process::Command::new("gh").output();',
            'let o = tokio::process::Command::new("git").output().await;',
            "use std::process::Command;",
            "use std::process::{Child, Command, Stdio};",
            "use tokio::process::Command as Run;",
            "use std::process::*;",
            "use std::os::unix::process::CommandExt;",
            'let o = Command::new("gh").into_std().output();',
            "let c = reqwest::Client::builder().build();",
            "let c = Client::builder().build();",
            "let r = reqwest::get(url).await;",
            "let s = std::net::TcpStream::connect(address);",
            "let s = tokio::net::TcpStream::connect(address).await;",
            "let s = UdpSocket::bind(address);",
        ]:
            self.write("src/a.rs", f"fn f() {{\n    {line}\n}}\n")
            self.assertEqual(len(guard.problems(self.root)), 1, line)

    def test_a_process_after_an_early_test_item_is_still_named(self):
        self.write("src/a.rs", '#[cfg(test)]\nuse std::fs;\n#[cfg(test)]\nfn t() { let _ = 1; }\nfn f() { std::process::Command::new("gh"); }\n')
        self.assertEqual(guard.problems(self.root), ["crates/daemon-engine/src/a.rs:5: a process command: use outside::Command or outside::AsyncCommand"])

    def test_going_through_the_door_passes(self):
        self.write("src/a.rs", 'use alondra_core::outside::{AsyncCommand, Command};\nfn f() { Command::new("git").output(); }\nfn h() -> reqwest::ClientBuilder { crate::web::client() }\n')
        self.assertEqual(guard.problems(self.root), [])

    def test_test_code_is_outside_the_guard(self):
        self.write("src/a_tests.rs", 'fn f() { std::process::Command::new("git"); }\n')
        self.write("tests/b.rs", 'fn f() { std::process::Command::new("git"); }\n')
        self.write("src/c.rs", 'fn f() {}\n#[cfg(test)]\nmod tests { fn t() { std::process::Command::new("git"); } }\nfn g() {}\n')
        self.write("src/d.rs", '#[cfg(any(test, feature = "test-hooks"))]\npub fn git() { std::process::Command::new("git"); }\n')
        self.write("src/e.rs", '#[cfg(test)]\n#[path = "e/fixture.rs"]\nmod fixture;\n')
        self.write("src/e/fixture.rs", 'fn f() { std::process::Command::new("git"); }\n')
        self.assertEqual(guard.problems(self.root), [])

    def test_the_doors_themselves_pass(self):
        self.write("src/outside.rs", "use std::process::Command;\n", crate="core")
        self.write("src/web.rs", "fn f() { reqwest::Client::builder(); }\n", crate="daemon-support")
        self.assertEqual(guard.problems(self.root), [])

    def test_a_comment_or_a_url_passes(self):
        self.write("src/a.rs", '// never std::process::Command here\nfn f() { let u = "https://example.com"; }\n')
        self.assertEqual(guard.problems(self.root), [])


if __name__ == "__main__":
    unittest.main()
