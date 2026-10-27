#!/usr/bin/env python3
"""The rehearsal's guards, HOME copy and upgrade sequence, with fake daemons and no network."""
from contextlib import closing
import importlib.util
import json
import os
import pathlib
import shutil
import socket
import sqlite3
import subprocess
import sys
import types
import tarfile
import tempfile
import time
import unittest
from unittest.mock import patch

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'upgrade-rehearsal.py'
SPEC = importlib.util.spec_from_file_location('upgrade_rehearsal', SCRIPT)
u = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(u)
REAL_POPEN = subprocess.Popen

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


def fake_launcher(path, version, padding=''):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(f"#!/bin/sh\necho '{u.LAUNCHER} {version}'\n# {padding}\n")
    path.chmod(0o755)
    return path


def printed_version(launcher):
    if not launcher.is_file():
        return None
    return REAL_POPEN([launcher, '--version'], stdout=subprocess.PIPE, text=True).communicate()[0].strip()


def fake_daemon(path, version, mode):
    path.write_text(FAKE_DAEMON.format(python=sys.executable, version=version, mode=mode))
    path.chmod(0o755)
    return path


class CopyHomeTests(unittest.TestCase):
    def test_a_database_that_cannot_be_read_in_place_is_copied_from_a_clone_of_it_and_its_wal(self):
        with tempfile.TemporaryDirectory() as directory:
            home, target = pathlib.Path(directory) / 'home', pathlib.Path(directory) / 'copy'
            home.mkdir()
            writer = sqlite3.connect(home / 'action-results.sqlite')
            try:
                writer.execute('PRAGMA journal_mode=WAL')
                writer.execute('PRAGMA wal_autocheckpoint=0')
                writer.execute('CREATE TABLE results(id INTEGER PRIMARY KEY, body TEXT)')
                writer.execute("INSERT INTO results(body) VALUES('only in the wal')")
                writer.commit()
                original = u.backup
                def refuse_in_place(source, destination):
                    if '?mode=ro' in source:
                        raise sqlite3.OperationalError('unable to open database file')
                    original(source, destination)
                with patch.object(u, 'backup', refuse_in_place):
                    u.copy_home(home, target)
            finally:
                writer.close()
            with closing(sqlite3.connect(target / 'action-results.sqlite')) as copy:
                self.assertEqual(copy.execute('SELECT body FROM results').fetchall(), [('only in the wal',)])
            self.assertFalse(any(path.name.endswith('.clone') for path in target.iterdir()), 'the clone is removed')


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
    def __init__(self, readiness_delay=0.2, installs_launcher=True, readiness_delays=None):
        self.now = 0.0
        self.readiness_delay = readiness_delay
        self.readiness_delays = readiness_delays or {}
        self.installs_launcher = installs_launcher
        self.processes = []
        self.waits = []

    def popen(self, argv, **kwargs):
        import ast
        from types import SimpleNamespace
        if argv[-1] == '--version':
            return REAL_POPEN(argv, **kwargs)
        launch = pathlib.Path(argv[-1])
        assignment = next(node for node in ast.parse(launch.read_text()).body
                          if isinstance(node, ast.Assign) and isinstance(node.targets[0], ast.Tuple))
        version, mode = ast.literal_eval(assignment.value)
        bundled = launch.with_name(u.LAUNCHER)
        installed = pathlib.Path(kwargs['env']['ALONDRA_HOME']) / 'bin' / u.LAUNCHER
        if self.installs_launcher and bundled.exists() and printed_version(bundled) != printed_version(installed):
            installed.parent.mkdir(exist_ok=True)
            shutil.copy(bundled, installed)
        process = SimpleNamespace(launch=launch, version=version, mode=mode,
                                  inode=launch.stat().st_ino, port=int(kwargs['env']['ALONDRA_PORT']),
                                  ready=self.now + self.readiness_delays.get(version, self.readiness_delay),
                                  returncode=1 if mode == 'crash' else None)
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
        status = {'version': process.version}
        if process.mode == 'reject' and process.launch.stat().st_ino != process.inode:
            status['preflight_rejected'] = {'reason': 'synthetic: bootstrap failed integrity audit'}
        return io.BytesIO(json.dumps(status).encode())

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


class HandoverWorldTests(Fixture):
    def test_the_daemon_of_a_case_starts_nodes_whatever_the_macs_load(self):
        world = u.HandoverWorld(self.rehearsal, self.work / 'case', None)
        self.addCleanup(world.supervisor.close)
        self.assertEqual(world.env().get('ALONDRA_IGNORE_MAC_LOAD'), '1',
                         'other builds keep the Mac busy: the gate must not hold the case\'s nodes')

    def test_the_claude_of_a_case_is_signed_in_so_the_daemon_has_an_account_to_start_on(self):
        tool = self.work / 'alondra-sandbox'
        tool.write_text('')
        world = u.HandoverWorld(self.rehearsal, self.work / 'case', types.SimpleNamespace(tool=tool))
        self.addCleanup(world.supervisor.close)
        world.root.mkdir()
        world.lay_out(None, world.root / 'Cellar/alondra/0.3.5', lambda keg: keg.mkdir(parents=True))
        self.assertEqual((world.claude_home / '.sandbox-account').read_text(), 'default@sandbox.test\n',
                         'the sandbox\'s claude says signed out without it, and a signed-out account starts nothing')


class HandoverCaseTests(unittest.TestCase):
    def test_only_a_release_before_the_agent_sdk_leaves_claude_bg_nodes_to_move(self):
        self.assertFalse(u.has_sdk_backend('0.2.91'))
        self.assertTrue(u.has_sdk_backend('0.3.0'), '0.3.0 launches no claude --bg node: the handover case cannot run from it')
        self.assertTrue(u.has_sdk_backend('0.3.1'))
        self.assertTrue(u.has_sdk_backend('1.0.0'))


class UpgradeCaseTests(Fixture):
    def test_a_release_with_the_agent_sdk_leaves_hosts_to_keep_and_one_before_it_claude_bg_nodes_to_move(self):
        self.assertEqual(u.sdk_cases('0.2.91'), (u.SDK_CASE, u.FRESH_SDK_CASE))
        self.assertEqual(u.sdk_cases('0.3.5'), (u.UPGRADE_CASE, u.FRESH_SDK_CASE))

    def test_only_the_named_cases_run_and_one_that_cannot_run_from_the_previous_release_is_refused(self):
        sdk = object()
        self.assertEqual(u.chosen_cases('0.3.5', sdk), [*u.CASES, u.UPGRADE_CASE, u.FRESH_SDK_CASE])
        self.assertEqual(u.chosen_cases('0.3.5', None), list(u.CASES))
        self.assertEqual(u.chosen_cases('0.3.5', sdk, [u.UPGRADE_CASE]), [u.UPGRADE_CASE])
        with self.assertRaisesRegex(u.Failed, f'{u.SDK_CASE} does not run when upgrading from 0.3.5'):
            u.chosen_cases('0.3.5', sdk, [u.SDK_CASE])

    def test_a_rehearsal_of_some_cases_writes_no_result_a_release_could_publish_with(self):
        run = subprocess.run([sys.executable, '-I', SCRIPT, '--new', self.work / 'alondrad', '--case', u.UPGRADE_CASE,
                              '--output', self.work / 'upgrade-rehearsal.json'], capture_output=True, text=True)
        self.assertEqual(run.returncode, 2, run.stderr)
        self.assertIn('leave out --output', run.stderr)
        self.assertFalse((self.work / 'upgrade-rehearsal.json').exists())

    def facts(self, command=(11, 'Thu Oct 22 10:00:00 2026'), host=(10, 'Thu Oct 22 09:59:00 2026'), release='0.3.5', ended=0, starts=1):
        return {'command': command, 'host': host, 'release': release, 'ended': ended, 'starts': [{'session': 's'}] * starts}

    def test_an_update_that_ends_the_hosts_with_their_commands_is_named_for_each_thing_it_lost(self):
        before = self.facts()
        self.assertEqual(u.kept(u.KEEPS, before, self.facts()), [])
        killed = self.facts(command=None, host=(20, 'Thu Oct 22 10:05:00 2026'), release='0.3.6', ended=1, starts=2)
        problems = u.kept(u.KEEPS, before, killed)
        self.assertEqual(len(problems), 4, problems)
        for said in ('command t-keeps left running in the background stopped', 'SDK host of t-keeps did not go on',
                     'retired 1 times', 'started 1 more times'):
            self.assertTrue(any(said in problem for problem in problems), (said, problems))
        reused = self.facts(command=(11, 'Thu Oct 22 10:07:00 2026'))
        self.assertEqual(len(u.kept(u.KEEPS, before, reused)), 1, 'a PID given to another process is not the command')

    def test_a_node_moved_once_a_host_of_another_release_holds_its_folder_and_the_old_one_is_gone(self):
        before = self.facts()
        new_host = (20, 'Thu Oct 22 10:05:00 2026')
        self.assertTrue(u.moved(before, self.facts(host=new_host, release='0.3.6', ended=1), old_host_runs=False))
        self.assertFalse(u.moved(before, self.facts(host=new_host, release='0.3.6', ended=1), old_host_runs=True))
        self.assertFalse(u.moved(before, self.facts(host=new_host, release='0.3.5', ended=1), old_host_runs=False))
        self.assertFalse(u.moved(before, self.facts(host=new_host, release=None, ended=1), old_host_runs=False))
        self.assertFalse(u.moved(before, self.facts(host=None, release='0.3.6', ended=1), old_host_runs=False))
        self.assertFalse(u.moved(before, self.facts(host=new_host, release='0.3.6', ended=0), old_host_runs=False))
        self.assertFalse(u.moved(before, self.facts(host=new_host, release='0.3.6', ended=2), old_host_runs=False),
                         'a second retired folder is a second replacement')

    def host_folder(self, root, name, session, release):
        folder = root / name
        folder.mkdir(parents=True)
        (folder / 'host.lock').write_text(json.dumps({'native': {'session_id': session}, 'manifest': {'host_release': release}}))
        return folder

    def test_a_sessions_host_folder_and_its_retired_ones_are_found_by_what_the_host_lock_holds(self):
        root = self.work / 'sdk'
        session, other = '0f0e0d0c-0000-4000-8000-000000000001', '0f0e0d0c-0000-4000-8000-000000000002'
        retired = self.host_folder(root, 'instance-a.ended-host-1', session, '0.3.5')
        live = self.host_folder(root, 'instance-a', session, '0.3.6')
        self.host_folder(root, 'instance-b', other, '0.3.6')
        (root / 'instance-c').mkdir()
        (root / 'instance-c/host.lock').write_text('{"native": {"sess')
        (root / 'stray-file').write_text('')
        self.assertEqual(u.session_hosts(root, session), (live, [retired]))
        self.assertEqual(u.session_hosts(root, '0f0e0d0c-0000-4000-8000-000000000003'), (None, []))
        self.assertEqual(u.session_hosts(self.work / 'missing', session), (None, []))
        self.assertEqual(u.bootstrap(root / 'instance-c'), None, 'a host.lock being written is no fact yet')

    def test_only_cards_about_a_lost_host_count_against_the_move(self):
        cards = [{'id': f'host-lost-{u.KEEPS}-7', 'task_id': u.KEEPS}, {'id': f'unheard-{u.KEEPS}-3', 'task_id': u.KEEPS},
                 {'id': f'session-ended-{u.CRASHES}-1-2', 'task_id': u.CRASHES}, {'id': f'handoff-doubt-{u.KEEPS}-1-input', 'task_id': u.KEEPS}]
        self.assertEqual(u.move_cards(cards, u.KEEPS), [f'handoff-doubt-{u.KEEPS}-1-input', f'host-lost-{u.KEEPS}-7'])
        self.assertEqual(u.move_cards(cards, u.CRASHES), [f'session-ended-{u.CRASHES}-1-2'])

    def test_each_turn_of_a_node_that_ends_leaves_a_card_of_its_own(self):
        cards = [{'id': 'turn-ended-t-keeps-1'}, {'id': 'turn-ended-t-keeps-2'}, {'id': 'host-lost-t-keeps-1'}]
        self.assertEqual(u.turns_ended(cards, 't-keeps'), 2)
        self.assertEqual(u.turns_ended(cards, 't-crashes'), 0)

    def stub(self, name):
        u.write_executable(self.work / name, f'#!{sys.executable}\nimport json, os, sys\n'
                           f'print(json.dumps({{"who": {name!r}, "args": sys.argv[1:], "capture": os.environ.get("FIXTURE_CAPTURE")}}))\n')
        return self.work / name

    def test_claude_on_the_path_sends_a_tasks_session_to_the_fixture_and_everything_else_to_the_sandbox(self):
        captures = self.work / 'captures'
        claude = self.work / 'claude'
        u.write_executable(claude, u.router(captures, self.stub('fixture'), self.stub('sandbox')))
        spawn = ['--output-format', 'stream-json', '--verbose', '--input-format', 'stream-json']
        session = '0f0e0d0c-0000-4000-8000-000000000001'

        def called(*args):
            run = subprocess.run([claude, *args], capture_output=True, text=True, env={'PATH': '/usr/bin:/bin'})
            self.assertEqual(run.returncode, 0, run.stderr)
            return json.loads(run.stdout)

        for args in ([*spawn, '--session-id', session, '--model', 'opus'], [*spawn, f'--resume={session}']):
            self.assertEqual(called(*args), {'who': 'fixture', 'args': args, 'capture': str(captures / f'capture-{session}')})
        for args in (['--version'], ['auth', 'status', '--json'], [*spawn, '--session-id', session, '--restricted'],
                     [*spawn, '--strict-mcp-config'], spawn):
            self.assertEqual(called(*args), {'who': 'sandbox', 'args': args, 'capture': None})

    def test_a_line_on_the_fifo_lets_the_background_command_end_and_without_one_waiting_the_case_fails(self):
        capture = self.work / 'capture-session'
        with self.assertRaisesRegex(u.Failed, 'no background command waits'):
            u.release_command(capture)
        os.mkfifo(f'{capture}.release')
        with self.assertRaisesRegex(u.Failed, 'no background command waits'):
            u.release_command(capture)
        reader = os.open(f'{capture}.release', os.O_RDONLY | os.O_NONBLOCK)
        self.addCleanup(os.close, reader)
        u.release_command(capture)
        self.assertEqual(os.read(reader, 64), b'done\n')

    def test_a_process_is_known_by_its_pid_and_start_and_is_gone_once_it_exits(self):
        child = subprocess.Popen(['/bin/cat'], stdin=subprocess.PIPE, stdout=subprocess.DEVNULL)
        self.addCleanup(child.wait)
        self.addCleanup(child.kill)
        seen = u.identity(child.pid)
        self.assertEqual(seen[0], child.pid)
        self.assertEqual(u.identity(child.pid), seen)
        self.assertEqual(u.parent(child.pid), os.getpid())
        child.stdin.close()
        child.wait()
        self.assertIsNone(u.identity(child.pid))
        self.assertIsNone(u.identity(None))

    def test_the_host_is_the_process_that_holds_its_lock(self):
        lock = self.work / 'host.lock'
        lock.write_text('{}')
        holder = subprocess.Popen([sys.executable, '-I', '-c', 'import fcntl, sys\nlock = open(sys.argv[1])\nfcntl.flock(lock, fcntl.LOCK_EX)\n'
                                   'print("held", flush=True)\nsys.stdin.read()', str(lock), 'alondra-host-fixture'],
                                  stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)
        self.addCleanup(holder.stdout.close)
        self.addCleanup(holder.wait)
        self.addCleanup(holder.kill)
        self.assertEqual(holder.stdout.readline(), 'held\n')
        with lock.open() as reader:
            self.assertEqual(u.lock_holders(lock), [u.identity(holder.pid)], 'only a process named as the host counts')
        holder.stdin.close()
        holder.wait()
        self.assertEqual(u.lock_holders(lock), [])


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

    def test_a_cases_processes_signal_each_other_through_one_supervisor_and_nothing_outside(self):
        supervisor = u.Supervisor(self.rehearsal)
        outside = subprocess.Popen(['/bin/sleep', '30'])
        try:
            env = {'PATH': '/usr/bin:/bin'}
            sleeper = supervisor.spawn(['/bin/sleep', '30'], env, self.work / 'sleep.log')
            self.assertIsNone(supervisor.exited(sleeper, 0))
            stopped = supervisor.run([sys.executable, '-I', '-c', f'import os; os.kill({sleeper}, 15)'], env, 10)
            self.assertEqual(stopped['code'], 0, stopped['stderr'])
            self.assertEqual(supervisor.exited(sleeper, 10), -15)
            refused = supervisor.run([sys.executable, '-I', '-c', f'import os; os.kill({outside.pid}, 15)'], env, 10)
            self.assertIn('PermissionError', refused['stderr'])
            self.assertIsNone(outside.poll())
        finally:
            supervisor.close()
            outside.kill()
            outside.wait()

    def test_separate_sandboxes_cannot_signal_each_other(self):
        first = subprocess.Popen(u.sandboxed(['/bin/sleep', '30'], self.protected, self.work))
        try:
            result = self.sandboxed(f'import os; os.kill({first.pid}, 15)')
            self.assertIn('PermissionError', result.stderr)
            self.assertIsNone(first.poll())
        finally:
            first.kill()
            first.wait()

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

    def old_and_new(self, mode, new_mode='serve', new_answers_as='1.0.1'):
        (self.root / 'bin').mkdir()
        return ({'version': '1.0.0', 'binary': fake_daemon(self.root / 'bin/old', '1.0.0', mode)},
                {'version': '1.0.1', 'binary': fake_daemon(self.root / 'bin/new', new_answers_as, new_mode)})

    def test_the_previous_daemon_restarts_into_the_new_one_through_the_opt_link(self):
        old, new = self.old_and_new('relaunch')
        changed = []
        machine = UpgradeMachine()
        with machine.boundaries(), patch.object(u, 'STAY_UP', 1):
            timings = u.upgrade(self.rehearsal, 'case', self.home, old, new, changed.append)
        self.assertEqual(changed, [self.home])
        self.assertEqual((self.work / 'case/opt/alondra').readlink(), self.work / 'case/Cellar/alondra/1.0.1')
        self.assertEqual(machine.waits, [('1.0.0', u.SERVE_WITHIN), ('1.0.1', u.SERVE_WITHIN)])
        self.assertEqual([process.version for process in machine.processes], ['1.0.0', '1.0.1'])
        self.assertEqual(machine.processes[0].launch, machine.processes[1].launch)
        self.assertEqual(machine.processes[0].port, machine.processes[1].port)
        self.assertEqual(machine.processes[0].returncode, 75)
        self.assertTrue(all(process.returncode is not None for process in machine.processes))

    def test_a_new_daemon_slowed_by_a_loaded_mac_passes_and_reports_how_long_it_took(self):
        old, new = self.old_and_new('relaunch')
        machine = UpgradeMachine(readiness_delays={'1.0.1': 5})
        with machine.boundaries(), patch.object(u, 'STAY_UP', 1):
            timings = u.upgrade(self.rehearsal, 'case', self.home, old, new)
        self.assertGreaterEqual(timings['answered_ms'], 5000)
        self.assertTrue(all(process.returncode is not None for process in machine.processes))

    def test_a_new_daemon_that_never_answers_fails(self):
        old, new = self.old_and_new('relaunch')
        machine = UpgradeMachine(readiness_delays={'1.0.1': u.SERVE_WITHIN + 1})
        with machine.boundaries(), self.assertRaisesRegex(u.Failed, f'did not answer as 1.0.1 within {u.SERVE_WITHIN} s'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)
        self.assertTrue(all(process.returncode is not None for process in machine.processes))

    def test_a_new_daemon_that_exits_fails(self):
        old, new = self.old_and_new('relaunch', new_mode='crash')
        with UpgradeMachine().boundaries(), self.assertRaisesRegex(u.Failed, 'the daemon exited 1'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)

    def test_a_new_daemon_that_answers_as_another_version_fails(self):
        old, new = self.old_and_new('relaunch', new_answers_as='1.0.0')
        with UpgradeMachine().boundaries(), self.assertRaisesRegex(u.Failed, 'did not answer as 1.0.1'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)

    def test_a_rejected_upgrade_fails_with_the_daemons_reason(self):
        old, new = self.old_and_new('reject')
        with UpgradeMachine().boundaries(), \
             self.assertRaisesRegex(u.Failed, 'did not restart into 1.0.1.*synthetic: bootstrap failed integrity audit'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)

    def test_a_daemon_that_never_notices_the_new_binary_fails(self):
        old, new = self.old_and_new('serve')
        with UpgradeMachine().boundaries(), self.assertRaisesRegex(u.Failed, 'did not restart into 1.0.1'):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)

    def test_no_daemon_outlives_the_rehearsal(self):
        old, new = self.old_and_new('serve')
        with patch.object(u, 'EXIT_WITHIN', 1), self.assertRaises(u.Failed):
            u.upgrade(self.rehearsal, 'case', self.home, old, new)
        deadline = time.monotonic() + 30
        while True:
            leftover = subprocess.run(['/usr/bin/pgrep', '-f', str(self.root / 'work/case/opt')], capture_output=True)
            if leftover.returncode == 1 or time.monotonic() > deadline:
                break
            time.sleep(0.05)
        self.assertEqual(leftover.returncode, 1, leftover.stdout)


class HandoverReadTests(Fixture):
    def database(self):
        home = self.work / 'home'
        home.mkdir()
        connection = sqlite3.connect(home / 'domain.db')
        for table in ('tasks', 'requests', 'interactions'):
            connection.execute(f'CREATE TABLE {table}(id TEXT PRIMARY KEY, state TEXT, title TEXT)')
            connection.execute(f'INSERT INTO {table} VALUES(?, ?, ?)', (f'{table}-1', 'open', 'One'))
        connection.commit()
        return home, connection

    def test_a_changed_task_request_or_card_is_named_with_its_fields(self):
        home, connection = self.database()
        before = u.domain_snapshot(home)
        self.assertEqual(u.snapshot_changes(before, u.domain_snapshot(home)), [])
        connection.execute("UPDATE tasks SET state='done'")
        connection.execute("INSERT INTO interactions VALUES('interactions-2', 'open', 'Two')")
        connection.commit()
        connection.close()
        self.assertEqual(u.snapshot_changes(before, u.domain_snapshot(home)),
                         ['interactions interactions-2: id, state, title', 'tasks tasks-1: state'])

    def test_each_move_lists_its_steps_in_order(self):
        home, connection = self.database()
        self.assertEqual(u.journal(home), {})
        connection.execute('CREATE TABLE sdk_handover_steps(node TEXT, step TEXT, task_id TEXT, at INTEGER, facts TEXT)')
        for node, step in (('b', 'planned'), ('a', 'done'), ('a', 'planned'), ('a', 'sdk_admitted'), ('a', 'bg_stopped')):
            connection.execute("INSERT INTO sdk_handover_steps VALUES(?, ?, 't', 0, '{}')", (node, step))
        connection.commit()
        connection.close()
        self.assertEqual(u.journal(home), {'a': list(u.HANDOVER_STEPS), 'b': ['planned']})


class LauncherMigrationTests(Fixture):
    def archive(self, files):
        path = self.root / 'release.tar.gz'
        with tarfile.open(path, 'w:gz') as output:
            for name in sorted(files):
                source = self.root / 'payload' / name
                source.parent.mkdir(parents=True, exist_ok=True)
                source.write_text(name)
                output.add(source, arcname=name)
        return path

    def test_the_published_release_may_predate_the_launcher(self):
        archive = self.archive(u.release.PRE_LAUNCHER_FILES)
        binary = u.unpack(archive, self.work / 'previous', signed=False, published=True)
        self.assertEqual(binary.read_text(), 'alondrad')
        self.assertFalse(binary.with_name(u.LAUNCHER).exists())

    def test_a_keg_holds_the_binaries_and_the_plugins_with_their_cli_wrappers(self):
        keg = self.work / 'Cellar/alondra/0.1.2'
        u.keg_from_archive(self.archive(u.release.PUBLIC_FILES), keg, published=False)
        self.assertEqual({path.name for path in (keg / 'bin').iterdir()}, {'alondra', 'alondrad', u.LAUNCHER})
        for plugin in ('plugin', 'plugin-orchestrator'):
            self.assertTrue((keg / 'libexec/alondra' / plugin / 'bin/alondra').is_file(), plugin)
        self.assertFalse((keg / 'libexec/alondra/alondrad').exists())
        self.assertTrue((keg / 'libexec/alondra' / u.release.HOST).is_file(), "the host is where the daemon looks for it")

    def test_the_release_about_to_ship_must_carry_the_launcher_and_the_sdk_host(self):
        for files, missing in ((u.release.PRE_LAUNCHER_FILES, "'alondrad-launcher'"), (u.release.PRE_SDK_FILES, "'sdk/alondra-host'")):
            with self.assertRaisesRegex(ValueError, f"Missing public files: .*{missing}"):
                u.unpack(self.archive(files), self.work / 'new', signed=False)
        binary = u.unpack(self.archive(u.release.PUBLIC_FILES), self.work / 'new', signed=False)
        self.assertEqual(binary.with_name(u.LAUNCHER).read_text(), u.LAUNCHER)

    def old_without_and_new_with_launcher(self):
        (self.root / 'old').mkdir()
        (self.root / 'new').mkdir()
        fake_launcher(self.root / 'new' / u.LAUNCHER, '1', padding='signed for 1.0.1')
        return ({'version': '1.0.0', 'binary': fake_daemon(self.root / 'old/alondrad', '1.0.0', 'relaunch')},
                {'version': '1.0.1', 'binary': fake_daemon(self.root / 'new/alondrad', '1.0.1', 'serve')})

    def home_with_launcher(self, version, padding):
        home = self.work / 'home'
        fake_launcher(home / 'bin' / u.LAUNCHER, version, padding)
        return home

    def bundled(self):
        return (self.work / 'case/Cellar/alondra/1.0.1/bin' / u.LAUNCHER).read_bytes()

    def test_the_new_daemon_puts_its_launcher_at_the_fixed_path(self):
        old, new = self.old_without_and_new_with_launcher()
        home = self.work / 'home'
        home.mkdir()
        with UpgradeMachine().boundaries(), patch.object(u, 'STAY_UP', 1):
            u.upgrade(self.rehearsal, 'case', home, old, new)
        self.assertFalse((self.work / 'case/Cellar/alondra/1.0.0/bin' / u.LAUNCHER).exists())
        self.assertEqual((home / 'bin' / u.LAUNCHER).read_bytes(), self.bundled())

    def test_a_launcher_of_the_same_version_signed_by_another_build_is_kept(self):
        old, new = self.old_without_and_new_with_launcher()
        home = self.home_with_launcher('1', padding='signed for 1.0.0')
        before = (home / 'bin' / u.LAUNCHER).read_bytes()
        with UpgradeMachine().boundaries(), patch.object(u, 'STAY_UP', 1):
            u.upgrade(self.rehearsal, 'case', home, old, new)
        self.assertEqual((home / 'bin' / u.LAUNCHER).read_bytes(), before)
        self.assertNotEqual(before, self.bundled())

    def test_an_outdated_launcher_is_replaced_by_the_one_the_release_ships(self):
        old, new = self.old_without_and_new_with_launcher()
        home = self.home_with_launcher('0', padding='an older launcher')
        with UpgradeMachine().boundaries(), patch.object(u, 'STAY_UP', 1):
            u.upgrade(self.rehearsal, 'case', home, old, new)
        self.assertEqual((home / 'bin' / u.LAUNCHER).read_bytes(), self.bundled())

    def test_an_upgrade_that_keeps_an_outdated_launcher_fails(self):
        old, new = self.old_without_and_new_with_launcher()
        home = self.home_with_launcher('0', padding='an older launcher')
        with UpgradeMachine(installs_launcher=False).boundaries(), patch.object(u, 'STAY_UP', 1), \
             self.assertRaisesRegex(u.Failed, f'1.0.1 left {u.LAUNCHER} 0 at .*; it ships {u.LAUNCHER} 1'):
            u.upgrade(self.rehearsal, 'case', home, old, new)

    def test_an_upgrade_that_leaves_no_launcher_fails(self):
        old, new = self.old_without_and_new_with_launcher()
        home = self.work / 'home'
        home.mkdir()
        with UpgradeMachine(installs_launcher=False).boundaries(), patch.object(u, 'STAY_UP', 1), \
             self.assertRaisesRegex(u.Failed, f'1.0.1 left no launcher at .*; it ships {u.LAUNCHER} 1'):
            u.upgrade(self.rehearsal, 'case', home, old, new)

    def test_the_outdated_launcher_case_leaves_a_launcher_older_than_any_release(self):
        home = self.work / 'home'
        home.mkdir()
        u.outdated_launcher(home)
        self.assertEqual(printed_version(home / 'bin' / u.LAUNCHER), f'{u.LAUNCHER} 0')


class PlistTests(unittest.TestCase):
    def test_launchd_restarts_the_daemon_on_every_exit(self):
        u.keep_alive_in_plist()


if __name__ == '__main__':
    unittest.main()
