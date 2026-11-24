#!/usr/bin/env python3
"""The bazel test TMPDIR wrapper leaves nothing of its own behind, however the test ends."""
import hashlib
import os
from pathlib import Path
import signal
import subprocess
import sys
import tempfile
import time
import unittest

wrapper = Path(__file__).resolve().parents[1] / "test-tmpdir.sh"

READ_ONLY_TREE = 'mkdir -p "$TMPDIR/cache/rules" && touch "$TMPDIR/cache/rules/BUILD" && chmod 555 "$TMPDIR/cache/rules"'


# Starts a process in its own group, as a host runs, and waits until it told its pid.
LEFT_RUNNING = """cd "$TMPDIR" && echo '{{}}' > {lock} && python3 -c '
import os, sys
os.setpgid(0, 0)
open(sys.argv[1], "w").write(str(os.getpid()))
os.execv("/bin/sleep", ["sleep", "600"])
' "{pid}" < {lock} >/dev/null 2>&1 &
until [ -s "{pid}" ]; do sleep 0.01; done
"""


def without_test_tmpdir():
    return {name: value for name, value in os.environ.items() if name != "TEST_TMPDIR"}


def running(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    return True


def end(pid):
    if pid.exists() and pid.read_text():
        try:
            os.kill(int(pid.read_text()), signal.SIGKILL)
        except ProcessLookupError:
            pass


def remove(path):
    for folder, _, _ in os.walk(path):
        os.chmod(folder, 0o700)
    subprocess.run(["rm", "-rf", str(path)], check=True)


GUARDED = unittest.skipUnless(sys.platform == "darwin", "the exec guard is the macOS kernel sandbox")


class BazelTestTmpdirTest(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="btt-"))
        self.addCleanup(remove, self.root)

    def environment(self, execs="own-sandbox", **more):
        environment = without_test_tmpdir()
        environment.update(
            BAZEL_TEST_TMPDIR_ROOT=str(self.root),
            ALONDRA_INVALID_EXECUTABLE_DIR=str(self.root / "kept"),
            TEST_EXECS=execs,
            TEST_TARGET="//crates/a:a_test",
        )
        environment.update(more)
        return environment

    def run_wrapped(self, script):
        return subprocess.run([str(wrapper), "/bin/sh", "-c", script], env=self.environment()).returncode

    def run_executable(self, contents, *arguments, input=None, **environment):
        executable = self.root / "test-executable"
        executable.write_bytes(contents)
        executable.chmod(0o755)
        return subprocess.run(
            [str(wrapper), str(executable), *arguments],
            env=self.environment(**environment),
            capture_output=True,
            input=input,
        )

    def run_guarded(self, script, execs="", target="//crates/a:a_test"):
        return subprocess.run(
            [str(wrapper), "/bin/sh", "-c", script],
            env=self.environment(execs, TEST_TARGET=target),
            capture_output=True,
            text=True,
        )

    def test_the_tests_tmpdir_is_a_short_private_folder_outside_bazels_and_both_go(self):
        given = self.root / "execroot" / "_main" / "_tmp" / ("0" * 32)
        given.mkdir(parents=True)
        report = self.root / "report"
        script = f'echo "$TMPDIR" > {report}; echo "$ALONDRA_SHORT_TMPDIR" >> {report}; touch "$TMPDIR/left-behind" "{given}/also"'
        result = subprocess.run([str(wrapper), "/bin/sh", "-c", script], env=self.environment(TEST_TMPDIR=str(given)))
        self.assertEqual(result.returncode, 0)
        used, short = report.read_text().split()
        self.assertEqual(used, short, "sockets and every other fixture share the one short folder")
        self.assertTrue(used.startswith((str(self.root / "bt."), str(self.root.resolve() / "bt."))), used)
        self.assertFalse(Path(used).resolve().is_relative_to(given.resolve()), "never inside the execroot, which links the checkout")
        self.assertFalse(Path(used).exists(), "the short folder goes with the test")
        self.assertTrue(given.is_dir() and not list(given.iterdir()), "what the test left in Bazel's folder goes too")

    @GUARDED
    def test_programs_the_test_writes_under_its_socket_folder_run_when_declared(self):
        given = self.root / "given"
        given.mkdir()
        script = 'printf "#!/bin/sh\\nexit 0\\n" > "$ALONDRA_SHORT_TMPDIR/fake" && chmod +x "$ALONDRA_SHORT_TMPDIR/fake" && "$ALONDRA_SHORT_TMPDIR/fake"'
        result = subprocess.run(
            [str(wrapper), "/bin/sh", "-c", script],
            env=self.environment("/usr/bin/printf /bin/chmod tmpdir", TEST_TMPDIR=str(given)),
            capture_output=True,
            text=True,
        )
        self.assertEqual(result.returncode, 0, result.stderr)

    @GUARDED
    def test_a_test_that_runs_an_undeclared_program_fails_and_says_how_to_see_it(self):
        result = self.run_guarded("/usr/bin/true")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("/usr/bin/true: Operation not permitted", result.stderr)
        self.assertIn("exec guard: this test may run only its own executable", result.stderr)
        self.assertIn("log stream", result.stderr)

    def test_a_declared_program_runs(self):
        result = self.run_guarded("/usr/bin/true", "/usr/bin/true")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("exec guard", result.stderr)

    @GUARDED
    def test_children_of_a_declared_program_are_guarded_too(self):
        result = self.run_guarded("/usr/bin/env /usr/bin/true", "/usr/bin/env")
        self.assertNotEqual(result.returncode, 0)

    @GUARDED
    def test_programs_the_test_writes_under_its_tmpdir_run_when_declared(self):
        script = 'printf "#!/bin/sh\\nexit 0\\n" > "$TMPDIR/fake" && chmod +x "$TMPDIR/fake" && "$TMPDIR/fake"'
        self.assertNotEqual(self.run_guarded(script, "/usr/bin/printf /bin/chmod").returncode, 0)
        self.assertEqual(self.run_guarded(script, "/usr/bin/printf /bin/chmod tmpdir").returncode, 0)

    def test_a_script_test_runs_its_interpreter_and_itself_again(self):
        script = f'#!{sys.executable}\nimport os, sys\nif len(sys.argv) == 1:\n    os.execv(sys.argv[0], [sys.argv[0], "again"])\nprint("ran", sys.argv[1])\n'.encode()
        result = self.run_executable(script, execs="")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout, b"ran again\n")

    @GUARDED
    def test_a_declared_set_user_id_program_runs(self):
        result = self.run_guarded("/bin/ps -p $$ >/dev/null", "/bin/ps")
        self.assertEqual(result.returncode, 0, result.stderr)

    @GUARDED
    def test_a_program_declared_by_name_runs_from_the_path(self):
        self.assertNotEqual(self.run_guarded("id -u").returncode, 0)
        result = self.run_guarded("id -u", "path:id")
        self.assertEqual(result.returncode, 0, result.stderr)

    @GUARDED
    def test_a_script_test_runs_what_its_launcher_needs_and_nothing_else(self):
        launcher = b'#!/usr/bin/env bash\nset -e\ndirname /a/b >/dev/null\nuname -s >/dev/null\n/usr/bin/true\n'
        result = self.run_executable(launcher, execs="")
        self.assertNotEqual(result.returncode, 0)
        self.assertIn(b"/usr/bin/true: Operation not permitted", result.stderr)
        self.assertNotIn(b"dirname: Operation not permitted", result.stderr)
        self.assertNotIn(b"uname: Operation not permitted", result.stderr)

    def test_swift_targets_doctests_and_end_to_end_tests_run_without_the_guard(self):
        self.assertEqual(self.run_guarded("/usr/bin/true", target="//apple/Feature:FeatureTests").returncode, 0)
        self.assertEqual(self.run_guarded("/usr/bin/true", "e2e").returncode, 0)
        doctest = self.root / "crate_test.rustdoc_test.sh"
        doctest.write_text("#!/bin/sh\n/usr/bin/true\n")
        doctest.chmod(0o755)
        result = subprocess.run([str(wrapper), str(doctest)], env=self.environment(""), capture_output=True, text=True)
        self.assertEqual(result.returncode, 0, result.stderr)

    def test_the_test_knows_it_runs_under_this_wrapper(self):
        self.assertEqual(self.run_wrapped('test "$ALONDRA_TEST_WRAPPER" = 1'), 0)

    def test_unrecognized_bytes_are_never_interpreted_as_shell(self):
        marker = self.root / "shell-fallback-ran"
        payload = f'touch "{marker}"\n'.encode().ljust(4016, b" ")
        result = self.run_executable(payload)
        self.assertEqual(result.returncode, 126)
        self.assertFalse(marker.exists())
        self.assertIn(b"Invalid test executable", result.stderr)
        self.assertIn(b"--noremote_accept_cached --disk_cache=", result.stderr)
        self.assertNotIn(b"make ", result.stderr)

    def test_an_invalid_executable_leaves_a_bounded_copy_that_expires(self):
        kept = self.root / "kept"
        kept.mkdir()
        old = kept / "old"
        old.write_bytes(b"x")
        os.utime(old, (time.time() - 8 * 86400,) * 2)
        contents = Path("/bin/ls").read_bytes()[:16]
        for number in range(4):
            result = self.run_executable(contents + bytes([number]))
            self.assertEqual(result.returncode, 126)
            time.sleep(1.1)
        copies = sorted(kept.iterdir())
        self.assertEqual(len(copies), 3, "at most three copies stay")
        self.assertFalse(old.exists(), "a copy older than a week goes")
        self.assertEqual(copies[-1].read_bytes(), contents + bytes([3]))
        self.assertIn(str(copies[-1]).encode(), result.stderr)

    def test_a_kept_copy_is_cut_at_the_size_limit(self):
        result = self.run_executable(b"\x00" * 5000, ALONDRA_INVALID_EXECUTABLE_MAX_BYTES="1000")
        self.assertEqual(result.returncode, 126)
        [copy] = (self.root / "kept").iterdir()
        self.assertEqual(copy.stat().st_size, 1000)

    def test_a_truncated_native_executable_is_rejected(self):
        result = self.run_executable(Path("/bin/ls").read_bytes()[:16])
        self.assertEqual(result.returncode, 126)
        self.assertIn(b"Invalid test executable", result.stderr)

    def test_an_invalid_executable_reports_its_size_digest_and_first_bytes(self):
        contents = Path("/bin/ls").read_bytes()[:16]
        result = self.run_executable(contents)
        self.assertEqual(result.returncode, 126)
        self.assertIn(b"16 bytes", result.stderr)
        self.assertIn(hashlib.sha256(contents).hexdigest().encode(), result.stderr)
        self.assertIn(contents.hex().encode(), result.stderr)
        self.assertIn(b"volume", result.stderr)

    def test_a_script_with_an_interpreter_keeps_its_arguments_and_exit_code(self):
        result = self.run_executable(b'#!/bin/sh\nprintf "%s" "$1"\nexit 7\n', "argument with spaces")
        self.assertEqual(result.returncode, 7)
        self.assertEqual(result.stdout, b"argument with spaces")

    def test_a_script_keeps_standard_input(self):
        result = self.run_executable(b"#!/bin/sh\nexec cat\n", input=b"test input\n")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout, b"test input\n")

    def test_a_native_executable_keeps_its_arguments_and_exit_code(self):
        result = subprocess.run(
            [str(wrapper), "test", "one", "=", "two"],
            env=dict(without_test_tmpdir(), BAZEL_TEST_TMPDIR_ROOT=str(self.root)),
            capture_output=True,
        )
        self.assertEqual(result.returncode, 1)
        self.assertEqual(result.stderr, b"")

    def test_a_passing_test_leaves_nothing_even_read_only_folders(self):
        self.assertEqual(self.run_wrapped(READ_ONLY_TREE), 0)
        self.assertEqual(list(self.root.iterdir()), [])

    @unittest.skipUnless(Path("/usr/bin/xcrun").exists(), "xcrun is macOS only")
    def test_a_tool_found_through_xcrun_from_a_new_home_caches_inside_the_test(self):
        env = {key: value for key, value in os.environ.items() if key not in ("xcrun_db", "TEST_TMPDIR")}
        env["BAZEL_TEST_TMPDIR_ROOT"] = str(self.root)
        script = 'mkdir "$TMPDIR/home" && HOME="$TMPDIR/home" /usr/bin/git --version >/dev/null && test -s "$TMPDIR/xcrun_db"'
        result = subprocess.run([str(wrapper), "/bin/sh", "-c", script], env=env)
        self.assertEqual(result.returncode, 0, "xcrun wrote its cache outside the test's TMPDIR")
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_failing_test_leaves_nothing_and_keeps_its_exit_code(self):
        self.assertEqual(self.run_wrapped(READ_ONLY_TREE + " && exit 3"), 3)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_test_cut_by_sigterm_leaves_nothing(self):
        started = self.root.parent / f"{self.root.name}.started"
        self.addCleanup(started.unlink, missing_ok=True)
        env = dict(without_test_tmpdir(), BAZEL_TEST_TMPDIR_ROOT=str(self.root))
        script = f'{READ_ONLY_TREE} && touch "{started}" && exec sleep 600'
        process = subprocess.Popen([str(wrapper), "/bin/sh", "-c", script], env=env)
        self.addCleanup(process.kill)
        while not started.exists():
            self.assertIsNone(process.poll(), "the wrapped test ended before it started")
            time.sleep(0.01)
        process.send_signal(signal.SIGTERM)
        self.assertEqual(process.wait(timeout=60), 143)
        self.assertEqual(list(self.root.iterdir()), [])

    def left_running(self, lock):
        pid = self.root.parent / f"{self.root.name}.pid"
        self.addCleanup(pid.unlink, missing_ok=True)
        self.addCleanup(end, pid)
        return pid, LEFT_RUNNING.format(lock=lock, pid=pid)

    def assert_ended(self, pid):
        while running(int(pid.read_text())):
            time.sleep(0.01)
        self.assertEqual(list(self.root.iterdir()), [])

    def test_a_passing_test_that_leaves_a_host_fails_and_the_host_ends(self):
        pid, script = self.left_running("host.lock")
        self.assertEqual(self.run_wrapped(script), 1)
        self.assert_ended(pid)

    def test_a_failing_test_that_leaves_a_host_keeps_its_exit_code_and_the_host_ends(self):
        pid, script = self.left_running("host.lock")
        self.assertEqual(self.run_wrapped(script + "exit 3"), 3)
        self.assert_ended(pid)

    def test_a_host_of_a_test_cut_by_sigterm_ends(self):
        pid, script = self.left_running("host.lock")
        env = dict(without_test_tmpdir(), BAZEL_TEST_TMPDIR_ROOT=str(self.root))
        process = subprocess.Popen([str(wrapper), "/bin/sh", "-c", script + "exec sleep 600"], env=env)
        self.addCleanup(process.kill)
        while not pid.exists() or not pid.read_text():
            self.assertIsNone(process.poll(), "the wrapped test ended before it started")
            time.sleep(0.01)
        process.send_signal(signal.SIGTERM)
        self.assertEqual(process.wait(timeout=60), 143)
        self.assert_ended(pid)

    def test_a_process_that_is_no_host_ends_without_failing_the_test(self):
        pid, script = self.left_running("input")
        self.assertEqual(self.run_wrapped(script), 0)
        self.assert_ended(pid)

    def test_folders_of_dead_wrappers_go_and_others_stay(self):
        dead = subprocess.Popen(["true"])
        dead.wait()
        orphan = self.root / "bt.orphan"
        (orphan / "cache/rules").mkdir(parents=True)
        (orphan / ".owner").write_text(f"{dead.pid}\n")
        (orphan / "cache/rules").chmod(0o555)
        alive = self.root / "bt.alive"
        alive.mkdir()
        (alive / ".owner").write_text(f"{os.getpid()}\n")
        unowned = self.root / "bt.unowned"
        unowned.mkdir()

        self.assertEqual(self.run_wrapped("true"), 0)
        self.assertEqual(sorted(p.name for p in self.root.iterdir()), ["bt.alive", "bt.unowned"])


if __name__ == "__main__":
    unittest.main()
