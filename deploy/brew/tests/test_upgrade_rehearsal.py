#!/usr/bin/env python3
"""The rehearsal's guards, HOME copy and upgrade sequence, with fake daemons and no network."""
from contextlib import closing
import importlib.util
import json
import os
import pathlib
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'upgrade-rehearsal.py'
SPEC = importlib.util.spec_from_file_location('upgrade_rehearsal', SCRIPT)
u = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(u)

FAKE_DAEMON = '''#!{python} -I
import http.server, json, os, sys, threading, time
VERSION, MODE = {version!r}, {mode!r}
launch = sys.argv[0]
def identity():
    meta = os.stat(launch)
    return meta.st_ino, meta.st_size, meta.st_mtime_ns
running = identity()
status = {{"version": VERSION}}
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        body = json.dumps(status).encode()
        self.send_response(200)
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", int(os.environ["ALONDRA_PORT"])), Handler)
threading.Thread(target=server.serve_forever, daemon=True).start()
while True:
    time.sleep(0.05)
    if MODE == "serve" or identity() == running:
        continue
    if MODE == "relaunch":
        print("replaced; passed preflight; restarting into it", flush=True)
        os._exit(75)
    status["preflight_rejected"] = {{"reason": "synthetic: bootstrap failed integrity audit"}}
'''


def fake_daemon(path, version, mode):
    path.write_text(FAKE_DAEMON.format(python=sys.executable, version=version, mode=mode))
    path.chmod(0o755)
    return path


class DaemonFixtureTests(unittest.TestCase):
    def test_replacement_after_readiness_is_not_used_as_the_initial_fingerprint(self):
        from types import SimpleNamespace
        from unittest.mock import Mock

        class StopLoop(Exception):
            pass

        generation = [1]
        def identity(_path):
            return SimpleNamespace(st_ino=generation[0], st_size=1, st_mtime_ns=1)
        def ready():
            generation[0] = 2

        thread = Mock()
        thread.start.side_effect = ready
        namespace = {}
        fixture = FAKE_DAEMON.format(python=sys.executable, version='1.0.0', mode='reject')
        with patch('http.server.HTTPServer'), patch('threading.Thread', return_value=thread), \
             patch('os.stat', side_effect=identity), patch.dict(os.environ, {'ALONDRA_PORT':'50000'}), \
             patch('time.sleep', side_effect=[None, StopLoop]):
            with self.assertRaises(StopLoop):
                exec(fixture, namespace)
        self.assertEqual(namespace['status'].get('preflight_rejected'),
            {'reason':'synthetic: bootstrap failed integrity audit'})


class UpgradeMachine:
    def __init__(self, readiness_delay=0.2):
        self.now = 0.0
        self.readiness_delay = readiness_delay
        self.processes = []
        self.waits = []

    def popen(self, argv, **kwargs):
        import ast
        from types import SimpleNamespace
        launch = pathlib.Path(argv[-1])
        assignment = next(node for node in ast.parse(launch.read_text()).body
                          if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Tuple))
        version, mode = ast.literal_eval(assignment.value)
        process = SimpleNamespace(launch=launch, version=version, mode=mode,
                                  inode=launch.stat().st_ino, port=int(kwargs['env']['ALONDRA_PORT']),
                                  ready=self.now + self.readiness_delay, returncode=None)
        process.poll = lambda: process.returncode
        def wait(timeout=None):
            if process.returncode is not None:
                return process.returncode
            if mode == 'relaunch' and launch.stat().st_ino != process.inode:
                process.returncode = u.RELAUNCH
                return process.returncode
            raise subprocess.TimeoutExpired(str(launch), timeout)
        def signal(_signal):
            process.returncode = -15
        process.wait = wait
        process.send_signal = signal
        process.kill = lambda: signal(9)
        self.processes.append(process)
        return process

    def urlopen(self, url, **kwargs):
        import io
        port = int(url.split(':')[2].split('/')[0])
        process = next(process for process in reversed(self.processes)
                       if process.port == port and process.returncode is None)
        if self.now < process.ready:
            raise OSError('synthetic startup is not ready')
        return io.BytesIO(json.dumps({'version': process.version}).encode())

    def sleep(self, duration):
        self.now += duration

    def boundaries(self):
        from contextlib import ExitStack
        stack = ExitStack()
        original = u.Daemon.wait_for_version
        def observed(daemon, version, within):
            self.waits.append((version, within))
            return original(daemon, version, within)
        for boundary in (patch.object(u.subprocess, 'Popen', side_effect=self.popen),
                         patch.object(u.urllib.request, 'urlopen', side_effect=self.urlopen),
                         patch.object(u, 'free_port', return_value=50000),
                         patch.object(u.time, 'monotonic', side_effect=lambda: self.now),
                         patch.object(u.time, 'sleep', side_effect=self.sleep),
                         patch.object(u.Daemon, 'wait_for_version', observed)):
            stack.enter_context(boundary)
        return stack


class Fixture(unittest.TestCase):
    def setUp(self):
        self.root = pathlib.Path(tempfile.mkdtemp(prefix='ur-test-', dir='/private/tmp')).resolve()
        self.protected = self.root / 'user'
        self.live = self.protected / '.alondra'
        self.live.mkdir(parents=True)
        self.work = self.root / 'work'
        self.work.mkdir()
        self.rehearsal = u.Rehearsal(self.work, self.protected, self.live)

    def tearDown(self):
        subprocess.run(['/bin/rm', '-rf', str(self.root)], check=True)


class SandboxTests(Fixture):
    def sandboxed(self, code):
        return subprocess.run(u.sandboxed([sys.executable, '-I', '-c', code], self.protected, self.work),
                              capture_output=True, text=True)

    def test_writes_reach_only_outside_the_protected_home(self):
        blocked = self.sandboxed(f'open({str(self.live / "x")!r}, "w")')
        self.assertNotEqual(blocked.returncode, 0)
        self.assertIn('Operation not permitted', blocked.stderr)
        self.assertFalse((self.live / 'x').exists())
        allowed = self.sandboxed(f'open({str(self.work / "x")!r}, "w")')
        self.assertEqual(allowed.returncode, 0, allowed.stderr)

    def test_only_loopback_is_reachable(self):
        with socket.socket() as server:
            server.bind(('127.0.0.1', 0))
            server.listen()
            port = server.getsockname()[1]
            loopback = self.sandboxed(f'import socket; socket.create_connection(("127.0.0.1", {port}), 2)')
        self.assertEqual(loopback.returncode, 0, loopback.stderr)
        outside = self.sandboxed('import socket; socket.create_connection(("192.0.2.1", 443), 2)')
        self.assertIn('PermissionError', outside.stderr)

    def test_unix_sockets_outside_the_rehearsal_are_unreachable(self):
        path = self.root / 's'
        with socket.socket(socket.AF_UNIX) as server:
            server.bind(str(path))
            server.listen()
            outside = self.sandboxed(f'import socket; socket.socket(socket.AF_UNIX).connect({str(path)!r})')
        self.assertIn('PermissionError', outside.stderr)

    def test_processes_outside_the_sandbox_cannot_be_signalled(self):
        sleeper = subprocess.Popen(['/bin/sleep', '30'])
        try:
            result = self.sandboxed(f'import os; os.kill({sleeper.pid}, 15)')
            self.assertIn('PermissionError', result.stderr)
            self.assertIsNone(sleeper.poll())
        finally:
            sleeper.kill()
            sleeper.wait()

    def test_the_rehearsal_directory_cannot_live_inside_the_protected_home(self):
        with self.assertRaisesRegex(u.Failed, 'must be outside'):
            u.sandboxed(['/usr/bin/true'], self.protected, self.live / 'work')


class GuardTests(Fixture):
    def test_a_daemon_never_runs_on_the_live_home_or_the_real_ports(self):
        with self.assertRaisesRegex(u.Failed, 'Refusing to start a daemon'):
            self.rehearsal.env(self.live, port=50000)
        with self.assertRaisesRegex(u.Failed, 'Refusing to start a daemon'):
            self.rehearsal.env(self.protected / 'other')
        for port in u.REAL_PORTS:
            with self.assertRaisesRegex(u.Failed, 'belongs to the real alondrad'):
                self.rehearsal.env(self.work / 'home', port=port)
        preflight = self.rehearsal.env(self.live)
        self.assertNotIn('XPC_SERVICE_NAME', preflight)
        daemon = self.rehearsal.env(self.work / 'home', port=50000)
        self.assertEqual(daemon['XPC_SERVICE_NAME'], u.LABEL)
        self.assertEqual(daemon['HOME'], str(self.work / 'fakehome'))
        self.assertTrue(daemon['PATH'].startswith(str(self.work / 'stubs')))
        self.assertEqual((daemon['ALONDRA_WAKE'], daemon['ALONDRA_AUTO_UPDATE']), ('0', '0'))

    def test_the_previous_version_is_the_newest_published_one_below_the_new(self):
        tags = ['alondra-v0.2.9', 'alondra-v0.2.10', 'alondra-v0.3.0', 'alondra-v0.2.11-rc1', 'other-v9.9.9']
        self.assertEqual(u.previous_version('0.2.11', tags), '0.2.10')
        self.assertEqual(u.previous_version('0.3.1', tags), '0.3.0')
        self.assertEqual(u.previous_version('latest', tags), '0.3.0')
        with self.assertRaisesRegex(u.Failed, 'No published release older'):
            u.previous_version('0.2.9', tags)

    def test_published_tags_leave_out_drafts(self):
        releases = [{'tag_name': 'alondra-v0.2.1', 'draft': False}, {'tag_name': 'alondra-v0.2.2', 'draft': True}]
        with patch.object(u.release, 'pages', return_value=releases):
            self.assertEqual(u.published_tags(), ['alondra-v0.2.1'])


class HomeTests(Fixture):
    def test_the_copy_reads_live_sqlite_and_drops_outward_files_sockets_and_logs(self):
        live_writer = sqlite3.connect(self.live / 'domain.db')
        live_writer.execute('PRAGMA journal_mode=WAL')
        live_writer.execute('PRAGMA wal_autocheckpoint=0')
        live_writer.execute('CREATE TABLE t(v)')
        live_writer.execute("INSERT INTO t VALUES ('only in the WAL')")
        live_writer.commit()
        closed = sqlite3.connect(self.live / 'domain.db.old-1')
        closed.execute('PRAGMA journal_mode=WAL')
        closed.execute('CREATE TABLE t(v)')
        closed.commit()
        closed.close()
        for name in ('service.toml', 'apns.json', 'alondrad.log', 'config.toml'):
            (self.live / name).write_text(name)
        (self.live / 'config.toml').chmod(0o640)
        (self.live / 'legacy-v1').mkdir()
        (self.live / 'legacy-v1/old.json').write_text('{}')
        hooks = socket.socket(socket.AF_UNIX)
        hooks.bind(str(self.live / 'hooks.sock'))
        try:
            self.rehearsal.copy(self.live, self.work / 'copy')
        finally:
            hooks.close()
            live_writer.close()
        copy = self.work / 'copy'
        with closing(sqlite3.connect(copy / 'domain.db')) as copied:
            rows = copied.execute('SELECT v FROM t').fetchall()
        self.assertEqual(rows, [('only in the WAL',)])
        self.assertTrue((copy / 'domain.db.old-1').exists())
        self.assertEqual((copy / 'legacy-v1/old.json').read_text(), '{}')
        self.assertEqual((copy / 'config.toml').stat().st_mode & 0o777, 0o640)
        for gone in ('service.toml', 'apns.json', 'alondrad.log', 'hooks.sock'):
            self.assertFalse((copy / gone).exists(), gone)

    def test_legacy_store_cases(self):
        home = self.work / 'home'
        home.mkdir()
        (home / 'alondra.db-journal').write_text('stale')
        u.empty_legacy_stores(home)
        self.assertEqual([(home / n).stat().st_size for n in ('alondra.db', 'schedules.db')], [0, 0])
        self.assertFalse((home / 'alondra.db-journal').exists())
        u.tableless_legacy_store(home)
        self.assertEqual((home / 'alondra.db').read_bytes()[:16], u.SQLITE_HEADER)
        with closing(sqlite3.connect(home / 'alondra.db')) as store:
            tables = store.execute('SELECT name FROM sqlite_master').fetchall()
        self.assertEqual(tables, [])

    def test_the_digest_tolerates_a_readers_sidecars_and_catches_writes(self):
        home = self.work / 'home'
        home.mkdir()
        (home / 'domain.db').write_bytes(b'data')
        before = u.digest(home)
        (home / 'domain.db-shm').write_bytes(b'index')
        (home / 'domain.db-wal').touch()
        self.assertEqual(u.digest(home), before)
        (home / 'domain.db-wal').write_bytes(b'frame')
        self.assertNotEqual(u.digest(home), before)
        (home / 'domain.db-wal').unlink()
        (home / 'domain.db').write_bytes(b'more data')
        self.assertNotEqual(u.digest(home), before)


class UpgradeTests(Fixture):
    def setUp(self):
        super().setUp()
        self.home = self.work / 'home'
        self.home.mkdir()

    def old_and_new(self, mode):
        (self.root / 'bin').mkdir()
        return ({'version': '1.0.0', 'binary': fake_daemon(self.root / 'bin/old', '1.0.0', mode)},
                {'version': '1.0.1', 'binary': fake_daemon(self.root / 'bin/new', '1.0.1', 'serve')})

    def test_the_previous_daemon_restarts_into_the_new_one_through_the_opt_link(self):
        old, new = self.old_and_new('relaunch')
        changed = []
        machine = UpgradeMachine()
        with machine.boundaries(), patch.object(u, 'STAY_UP', 1):
            timings = u.upgrade(self.rehearsal, 'case', self.home, old, new, changed.append)
        self.assertEqual(changed, [self.home])
        self.assertLess(timings['answered_ms'], u.HEALTH_WITHIN * 1000)
        self.assertEqual((self.work / 'case/opt/alondra').readlink(), self.work / 'case/Cellar/alondra/1.0.1')
        self.assertEqual(machine.waits, [('1.0.0', u.SERVE_WITHIN), ('1.0.1', 1.0)])
        self.assertEqual([process.version for process in machine.processes], ['1.0.0', '1.0.1'])
        self.assertEqual(machine.processes[0].launch, machine.processes[1].launch)
        self.assertEqual(machine.processes[0].port, machine.processes[1].port)
        self.assertEqual(machine.processes[0].returncode, 75)
        self.assertTrue(all(process.returncode is not None for process in machine.processes))

    def test_the_new_daemon_must_answer_within_one_second_under_a_controlled_clock(self):
        old, new = self.old_and_new('relaunch')
        machine = UpgradeMachine(readiness_delay=1.5)
        with machine.boundaries(), self.assertRaisesRegex(u.Failed, 'did not answer as 1.0.1 within 1.0 s'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)
        self.assertEqual(machine.waits[-1], ('1.0.1', 1.0))
        self.assertTrue(all(process.returncode is not None for process in machine.processes))

    def test_a_rejected_upgrade_fails_with_the_daemons_reason(self):
        old, new = self.old_and_new('reject')
        with patch.object(u, 'EXIT_WITHIN', 2), \
             self.assertRaisesRegex(u.Failed, 'did not restart into 1.0.1.*synthetic: bootstrap failed integrity audit'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)

    def test_a_daemon_that_never_notices_the_new_binary_fails(self):
        old, new = self.old_and_new('serve')
        with patch.object(u, 'EXIT_WITHIN', 1), self.assertRaisesRegex(u.Failed, 'did not restart into 1.0.1'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)

    def test_no_daemon_outlives_the_rehearsal(self):
        old, new = self.old_and_new('serve')
        with patch.object(u, 'EXIT_WITHIN', 1), self.assertRaises(u.Failed):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)
        time.sleep(0.2)
        leftover = subprocess.run(['/usr/bin/pgrep', '-f', str(self.root / 'work/case/opt')], capture_output=True)
        self.assertEqual(leftover.returncode, 1, leftover.stdout)


class PlistTests(unittest.TestCase):
    def test_launchd_restarts_the_daemon_on_every_exit(self):
        u.keep_alive_in_plist()


if __name__ == '__main__':
    unittest.main()
