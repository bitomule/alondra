#!/usr/bin/env python3
"""Benchmark oracles and lifecycle, with no services, sockets or installed Alondra."""
from contextlib import contextmanager, redirect_stdout
import importlib.util
import io
import json
from pathlib import Path
import subprocess
import threading
import sys
import unittest
from unittest.mock import Mock, patch
import urllib.error

module_path = Path(__file__).resolve().parents[1] / "load_bench.py"
spec = importlib.util.spec_from_file_location("load_bench", module_path)
bench = importlib.util.module_from_spec(spec)
spec.loader.exec_module(bench)


class Response(io.BytesIO):
    def __init__(self, body, status=200):
        super().__init__(body)
        self.status = status


class Process:
    pid = 123

    def __init__(self, env):
        self.env = env
        self.exited = False
        home = Path(env["ALONDRA_HOME"])
        assert home.name.startswith("alondra-load-bench-")
        (home / "hooks.sock").touch()

    def poll(self):
        return 0 if self.exited else None

    def terminate(self):
        self.exited = True

    def kill(self):
        self.exited = True

    def wait(self, timeout=None):
        return 0


class IdleThread:
    def __init__(self, **kwargs):
        pass

    def start(self):
        pass

    def join(self, timeout=None):
        pass

    def is_alive(self):
        return False


class BenchmarkTests(unittest.TestCase):
    @contextmanager
    def peer(self, effect):
        open_request = Mock(side_effect=effect)
        with patch.object(bench.urllib.request, "urlopen", open_request), \
             patch.object(bench.urllib.request, "build_opener", return_value=Mock(open=open_request)):
            yield open_request

    def test_probe_uses_the_current_route_and_closes_its_response(self):
        response = Response(b'{"version":"0.2.56"}')
        with self.peer(lambda *args, **kwargs: response) as opener:
            self.assertTrue(bench.health(12345))
        opener.assert_called_once_with("http://127.0.0.1:12345/daemon", timeout=30)
        self.assertTrue(response.closed)

    def test_http_failures_are_never_healthy(self):
        for code in (401, 404, 429, 500, 503):
            with self.subTest(status=code):
                error = urllib.error.HTTPError("http://127.0.0.1:1/daemon", code, "synthetic", None, io.BytesIO(b"synthetic error"))
                with self.peer(error):
                    self.assertFalse(bench.health(1))
                self.assertTrue(error.closed)

    def test_probe_requires_a_valid_bounded_daemon_reply(self):
        bodies = (b'{"ok":true}', b'{"version":""}', b'[]', b'{',
                  json.dumps({"version": "0.2.56", "padding": "x" * (64 * 1024)}).encode())
        for body in bodies:
            with self.subTest(length=len(body)):
                response = Response(body)
                with self.peer(lambda *args, **kwargs: response):
                    self.assertFalse(bench.health(1))
                self.assertTrue(response.closed)

    def test_fast_errors_cannot_improve_success_percentiles(self):
        result = bench.summary([(0.001, False)] * 99 + [(10.0, True)])
        self.assertEqual(result["p95_ms"], 10_000)
        self.assertEqual(result["n"], 100)
        self.assertEqual(result["failed"], 99)
        self.assertEqual(result["accepted_n"], 1)
        self.assertEqual(result["error_latency"]["p95_ms"], 1)

    def test_quantiles_use_nearest_rank_among_successes(self):
        result = bench.summary([(0.001, True), (0.002, True), (0.003, True), (0.004, True)])
        self.assertEqual((result["p50_ms"], result["p95_ms"], result["p99_ms"]), (2, 4, 4))

    def test_no_success_is_not_a_zero_latency_success(self):
        for samples in ([], [(0.001, False), (0.002, False)]):
            with self.subTest(samples=samples):
                result = bench.summary(samples)
                self.assertIsNone(result.get("p95_ms"))
                self.assertEqual(result["accepted_n"], 0)

    def test_listener_identity_requires_the_owned_pid_and_a_successful_query(self):
        for code, reply, expected in ((0, "p123\n", True), (0, "p456\n", False),
                                       (1, "p123\n", False), (0, "", False)):
            with self.subTest(code=code, reply=reply), patch.object(bench.subprocess, "run",
                    return_value=subprocess.CompletedProcess([], code, stdout=reply)) as query:
                self.assertEqual(bench.owns_listener(123, 4567), expected)
                self.assertIn("-p", query.call_args.args[0])
                self.assertIn("123", query.call_args.args[0])
                self.assertIn("-iTCP:4567", query.call_args.args[0])
        with patch.object(bench.subprocess, "run", side_effect=subprocess.TimeoutExpired("lsof", 3)):
            self.assertFalse(bench.owns_listener(123, 4567))

    def test_verdict_rejects_errors_in_any_cohort_and_unfinished_workers(self):
        result = {name: bench.summary([(0.001, True)]) for name in ("health", "view", "hook")}
        self.assertTrue(bench.verdict(result)["passed"])
        for name in result:
            failed = dict(result, **{name: bench.summary([(0.001, True), (0.002, False)])})
            self.assertFalse(bench.verdict(failed)["passed"])
        self.assertFalse(bench.verdict(dict(result, unfinished_workers=1))["passed"])

    def test_a_truncated_or_malformed_hook_reply_is_never_successful(self):
        for reply in (b"", b"\x00", b"\x00\xc8", b"\x00\xc8{", b"\x00\x00{}"):
            with self.subTest(reply=reply):
                connection = Mock()
                connection.recv.side_effect = [reply, b""]
                context = Mock()
                context.__enter__ = Mock(return_value=connection)
                context.__exit__ = Mock(return_value=False)
                with patch.object(bench.socket, "socket", return_value=context):
                    self.assertFalse(bench.frame("/synthetic", {"probe": True}))
                self.assertNotIn(unittest.mock.call(b"\x06"), connection.sendall.call_args_list)

    def test_a_complete_fragmented_hook_reply_is_acknowledged(self):
        connection = Mock()
        connection.recv.side_effect = [b"\x00", b"\xc8{", b"}", b""]
        context = Mock()
        context.__enter__ = Mock(return_value=connection)
        context.__exit__ = Mock(return_value=False)
        with patch.object(bench.socket, "socket", return_value=context):
            self.assertTrue(bench.frame("/synthetic", {"probe": True}))
        self.assertEqual(connection.sendall.call_args_list[-1], unittest.mock.call(b"\x06"))

    def test_offered_work_is_visible_before_completion(self):
        records = []
        def action():
            self.assertEqual(len(records), 1)
            self.assertIsNone(records[0]["finished"])
            return True
        with patch.object(bench.time, "monotonic", side_effect=[101.0, 102.0]):
            bench.timed(records, threading.Lock(), action)
        self.assertEqual(records, [{"started": 101.0, "finished": 102.0, "ok": True, "error": None}])

    def test_an_exception_is_a_completed_failure_not_missing_work(self):
        records = []
        with patch.object(bench.time, "monotonic", side_effect=[101.0, 102.0]):
            bench.timed(records, threading.Lock(), Mock(side_effect=OSError("synthetic")))
        self.assertFalse(records[0]["ok"])
        self.assertEqual(records[0]["error"], "OSError")
        self.assertEqual(records[0]["finished"], 102.0)

    def test_measurement_separates_warmup_steady_drain_and_unfinished_work(self):
        records = [
            {"started": 99.0, "finished": 101.0, "ok": True},
            {"started": 101.0, "finished": 102.0, "ok": True},
            {"started": 119.0, "finished": 121.0, "ok": False},
            {"started": 120.0, "finished": 121.0, "ok": True},
            {"started": 110.0, "finished": None, "ok": None},
        ]
        result = bench.window_summary(records, 100.0, 120.0)
        self.assertEqual(result["offered"], 3)
        self.assertEqual(result["n"], 1)
        self.assertEqual(result["p95_ms"], 1000)
        self.assertEqual(result["pending_at_boundary"], 2)
        self.assertEqual(result["drain"]["n"], 1)
        self.assertEqual(result["drain"]["failed"], 1)
        self.assertEqual(result["unfinished"], 1)
        self.assertEqual(result["excluded"], 2)

    def test_drain_errors_reports_and_worker_exceptions_make_the_run_fail(self):
        healthy = bench.window_summary([
            {"started": 101.0, "finished": 102.0, "ok": True}], 100.0, 120.0)
        result = {name: healthy for name in ("health", "view", "hook", "report")}
        self.assertTrue(bench.verdict(result)["passed"])
        late = dict(healthy, drain=bench.summary([(2.0, False)]))
        self.assertFalse(bench.verdict(dict(result, view=late))["passed"])
        self.assertFalse(bench.verdict(dict(result, report=bench.summary([(1.0, False)])))["passed"])
        self.assertFalse(bench.verdict(dict(result, worker_failures=["mod: OSError"]))["passed"])
        self.assertFalse(bench.verdict(dict(result, hook=dict(healthy, unfinished=1)))["passed"])

    def test_worker_exceptions_are_recorded(self):
        failures = []
        bench.run_worker(failures, threading.Lock(), "mod-0", Mock(side_effect=RuntimeError("synthetic")))
        self.assertEqual(failures, ["mod-0: RuntimeError"])

    def test_persistent_mod_workers_are_tracked_and_joined(self):
        created = []
        class TrackedThread(IdleThread):
            def __init__(self, **kwargs):
                self.label = kwargs["args"][2]
                self.joins = 0
                created.append(self)
            def join(self, timeout=None):
                self.joins += 1
        with self.run_fixture() as (_, output), patch.object(bench.threading, "Thread", TrackedThread):
            with self.assertRaises(SystemExit):
                bench.main()
        self.assertEqual([worker.label for worker in created], ["mod-0", "node-0", "client-0", "prober"])
        self.assertTrue(all(worker.joins for worker in created))
        result = json.loads(output.getvalue())
        self.assertEqual(result["phases"], {"warmup_seconds": 5, "steady_seconds": 20.0, "drain_seconds": 1.0})
        self.assertEqual(result["report"]["offered"], 0)

    @contextmanager
    def run_fixture(self, initial_status=True):
        processes = []
        output = io.StringIO()

        def spawn(argv, **kwargs):
            process = Process(kwargs["env"])
            processes.append(process)
            return process

        argv = ["load_bench", "--daemon", "fixture-daemon", "--cli", "fixture-cli",
                "--seed", "fixture-seed", "--nodes", "1", "--clients", "1", "--seconds", "20"]
        with patch.object(sys, "argv", argv), \
             patch.object(bench.subprocess, "run", return_value=subprocess.CompletedProcess([], 0)), \
             patch.object(bench.subprocess, "Popen", side_effect=spawn), \
             patch.object(bench.threading, "Thread", IdleThread), \
             patch.object(bench.time, "sleep"), \
             patch.object(bench.time, "monotonic", side_effect=[100.0, 120.0, 121.0]), \
             patch.object(bench, "cpu_seconds", return_value=0), \
             patch.object(bench, "owns_listener", return_value=True, create=True), \
             patch.object(bench, "health", return_value=True), \
             patch.object(bench, "status", return_value=initial_status), \
             patch.object(bench.os, "getloadavg", return_value=(0, 0, 0)), \
             redirect_stdout(output):
            yield processes, output

    def test_main_fails_when_workers_complete_no_work(self):
        with self.run_fixture() as (processes, output):
            with self.assertRaises(SystemExit) as stopped:
                bench.main()
            self.assertEqual(stopped.exception.code, 1)
            result = json.loads(output.getvalue())
            self.assertFalse(result["verdict"]["passed"])
        self.assertIsNotNone(processes[0].poll())
        self.assertEqual(processes[0].env["HOME"], processes[0].env["ALONDRA_HOME"])

    def test_invalid_load_configuration_is_rejected_before_starting_a_daemon(self):
        for flag, value in (("--nodes", "0"), ("--clients", "0"), ("--seconds", "0"),
                            ("--burn", "-1"), ("--hook-every", "0")):
            with self.subTest(flag=flag), self.run_fixture() as (processes, _):
                argv = list(sys.argv)
                if flag in argv:
                    argv[argv.index(flag) + 1] = value
                else:
                    argv += [flag, value]
                with patch.object(sys, "argv", argv), redirect_stdout(io.StringIO()), \
                     patch.object(sys, "stderr", io.StringIO()), self.assertRaises(SystemExit) as stopped:
                    bench.main()
                self.assertEqual(stopped.exception.code, 2)
                self.assertEqual(processes, [])

    def test_a_new_run_cannot_inherit_previous_failure_messages(self):
        bench.FAILURES.append("a failure from an earlier run")
        with self.run_fixture() as (_, output):
            with self.assertRaises(SystemExit):
                bench.main()
            self.assertEqual(json.loads(output.getvalue())["view_failures"], [])

    def test_partial_burner_creation_cleans_every_owned_process(self):
        class Burner:
            exited = False
            def poll(self): return 0 if self.exited else None
            def terminate(self): self.exited = True
            def kill(self): self.exited = True
            def wait(self, timeout=None): return 0
        burner = Burner()
        calls = 0
        with self.run_fixture() as (processes, _):
            outer_spawn = bench.subprocess.Popen
            def spawn(argv, **kwargs):
                nonlocal calls
                if argv[0] == "fixture-daemon": return outer_spawn(argv, **kwargs)
                calls += 1
                if calls == 1: return burner
                raise OSError("synthetic process exhaustion")
            with patch.object(sys, "argv", list(sys.argv) + ["--burn", "2"]), \
                 patch.object(bench.subprocess, "Popen", side_effect=spawn):
                with self.assertRaisesRegex(OSError, "synthetic process exhaustion"):
                    bench.main()
        self.assertTrue(burner.exited)
        self.assertIsNotNone(processes[0].poll())
        self.assertFalse(Path(processes[0].env["HOME"]).exists())

    def test_a_worker_start_failure_is_not_masked_by_joining_unstarted_workers(self):
        created = []
        class StartFailure(IdleThread):
            def __init__(self, **kwargs):
                self.started = False
                created.append(self)
            def start(self):
                if created.index(self) == 1: raise RuntimeError("synthetic start failure")
                self.started = True
            def join(self, timeout=None):
                if not self.started: raise RuntimeError("cannot join thread before it is started")
        with self.run_fixture() as (processes, _), patch.object(bench.threading, "Thread", StartFailure):
            with self.assertRaisesRegex(RuntimeError, "synthetic start failure"):
                bench.main()
        self.assertIsNotNone(processes[0].poll())
        self.assertFalse(Path(processes[0].env["HOME"]).exists())

    def test_startup_failure_terminates_only_the_owned_daemon(self):
        with self.run_fixture(initial_status=False) as (processes, _):
            with self.assertRaises(SystemExit):
                bench.main()
        self.assertEqual(len(processes), 1)
        self.assertIsNotNone(processes[0].poll())
        self.assertFalse(Path(processes[0].env["ALONDRA_HOME"]).exists())


if __name__ == "__main__":
    unittest.main()
