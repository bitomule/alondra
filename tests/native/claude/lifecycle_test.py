"""Owned Python stand-ins test only fixture teardown, never SDK/native behavior."""
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('native_harness', Path(__file__).with_name('run.py'))
harness = importlib.util.module_from_spec(spec)
spec.loader.exec_module(harness)

DESCENDANT_FAKE_CLAUDE = Path(os.environ['DESCENDANT_FAKE_CLAUDE']).resolve(strict=True)

# Stands in for the Node SDK client: its Claude leaves a descendant in the leader's group, then the
# leader ends without running its finally (crash) or exits cleanly without stopping it (exit).
LEADER = r'''
import json, os, signal, subprocess, sys
claude = subprocess.Popen([sys.executable, '-I', sys.argv[1], '--session-id=00000000-0000-4000-8000-000000000300'],
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE)
def send(value):
    claude.stdin.write((json.dumps(value) + '\n').encode())
    claude.stdin.flush()
def until(kind):
    while json.loads(claude.stdout.readline())['type'] != kind:
        pass
try:
    send({'type': 'control_request', 'request_id': 'init', 'request': {'subtype': 'initialize'}})
    until('system')
    send({'type': 'user', 'uuid': '00000000-0000-4000-8000-000000000301', 'message': {'role': 'user', 'content': 'fixture'}})
    until('result')
    print('ready', flush=True)
    if sys.argv[2] == 'crash':
        os.kill(os.getpid(), signal.SIGKILL)
    os._exit(0)
finally:
    claude.kill()
'''


def await_completion(state):
    # The descendant holds this lock until the kernel closes its files, which can follow NOTE_EXIT.
    with open(state / 'completion', 'rb') as completion:
        fcntl.flock(completion, fcntl.LOCK_EX)


class OwnedClientTeardown(unittest.TestCase):
    def exercise(self, after_started, expected, running=True):
        with tempfile.TemporaryDirectory(dir=os.environ['TEST_TMPDIR']) as directory:
            config = {'work': directory, 'env': {'HOME': directory, 'PATH': '/usr/bin:/bin', 'PYTHONNOUSERSITE': '1'}}
            script = 'import signal; print("ready", flush=True); ' + ('signal.pause()' if running else 'raise SystemExit(0)')
            children, observed = [], {}
            def ready(child):
                children.append(child)
                self.assertEqual(child.stdout.readline(), b'ready\n')
                after_started(child)
            try:
                with harness.Interruptions() as interruptions:
                    if expected is None:
                        result = harness._owned_client([sys.executable, '-I', '-u', '-c', script], config, subprocess.PIPE, subprocess.PIPE, interruptions, observed, ready)
                        self.assertEqual(result, 0)
                    else:
                        with self.assertRaises(type(expected)) as failure:
                            harness._owned_client([sys.executable, '-I', '-u', '-c', script], config, subprocess.PIPE, subprocess.PIPE, interruptions, observed, ready)
                        if not isinstance(expected, harness.HarnessInterrupted):
                            self.assertIs(failure.exception, expected, 'Cleanup must preserve the original failure object')
                    self.assertTrue(observed['client_exit_waited'], 'Interruption/error must positively wait for its actual owned client')
                    self.assertEqual(observed['group_survivors'], [])
                    self.assertEqual(len(children), 1)
                    with self.assertRaises(ChildProcessError):
                        os.waitpid(children[0].pid, os.WNOHANG)
                    return observed, list(interruptions.received)
            finally:
                # Also owns cleanup when a deliberate guard ablation makes the assertions fail.
                for child in children:
                    if child.poll() is None:
                        child.kill()
                    child.wait()
                    child.stdout.close()
                    child.stderr.close()

    def abandoned_descendant(self, ending):
        state = Path(tempfile.mkdtemp(prefix='nc-', dir='/tmp')).resolve()
        self.addCleanup(shutil.rmtree, state)
        config = {'work': str(state), 'env': {'HOME': str(state), 'PATH': '/usr/bin:/bin', 'FIXTURE_STATE': str(state)}}
        children, descendant, observed, failure = [], {}, {}, None
        def ready(child):
            children.append(child)
            self.assertEqual(child.stdout.readline(), b'ready\n')
            captured = json.loads((state / 'capture.json').read_text())['descendant_pid']
            descendant.update(harness.process_identity(captured) or {})
            self.assertEqual(descendant.get('pgid'), child.pid, 'The descendant must start in the leader group')
        self.addCleanup(self.release, children, state, descendant)
        with harness.Interruptions() as interruptions:
            try:
                harness._owned_client([sys.executable, '-I', '-u', '-c', LEADER, str(DESCENDANT_FAKE_CLAUDE), ending], config, subprocess.PIPE, subprocess.PIPE, interruptions, observed, ready)
            except RuntimeError as error:
                failure = error
        self.assertTrue(descendant, 'The leader must report its live descendant first')
        self.assertNotEqual(harness.process_identity(descendant['pid']), descendant, 'descendant-fake-claude descendant ' + harness.describe(descendant) + ' survived the leader')
        await_completion(state)
        self.assertEqual(observed['group_survivors'], [descendant])
        self.assertIn('Owned client left live process group members: ' + harness.describe(descendant), str(failure))
        self.assertTrue(observed['client_exit_waited'])
        return observed

    def release(self, children, state, descendant):
        # A survivor (guard ablation) is stopped through its own socket, never by a PID that may be reused.
        if descendant and harness.process_identity(descendant['pid']) == descendant:
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
                connection.connect(str(state / 'd'))
                connection.sendall(b'stop\n')
                connection.recv(64)
            await_completion(state)
        for child in children:
            child.wait()
            child.stdout.close()
            child.stderr.close()

    def test_leader_killed_before_its_finally_leaves_no_live_descendant(self):
        observed = self.abandoned_descendant('crash')
        self.assertEqual(observed['client_exit_code'], -signal.SIGKILL)

    def test_clean_leader_exit_that_abandons_a_descendant_fails_and_kills_it(self):
        observed = self.abandoned_descendant('exit')
        self.assertEqual(observed['client_exit_code'], 0)

    def test_group_is_never_signalled_after_its_leader_is_reaped(self):
        child = subprocess.Popen(['/usr/bin/true'], start_new_session=True)
        child.wait()
        with self.assertRaises(RuntimeError):
            harness._signal_group(child)

    def test_sandbox_profile_stays_deny_default_without_mach_lookup(self):
        profile = harness.sandbox_profile(Path('/fixture/runtime'), 4242).splitlines()
        self.assertEqual(profile[:3], ['(version 1)', '(deny default)', '(deny mach-lookup)'])
        self.assertIn('(allow signal (target same-sandbox))', profile)
        self.assertEqual([line for line in profile if 'network' in line], ['(allow network-outbound (remote ip "localhost:4242"))'])
        self.assertIn('(allow file-read-data (literal "/"))', profile)
        self.assertIn('(allow file-read-metadata (literal "/fixture") (literal "/"))', profile)
        self.assertEqual([line for line in profile if line.startswith('(allow file-read') and '(subpath "/")' in line], [])

    def test_keyboard_interrupt_keeps_primary_and_reaps_actual_client(self):
        primary = KeyboardInterrupt('fixture interruption')
        def interrupt(_child):
            raise primary
        observed, _ = self.exercise(interrupt, primary)
        self.assertLess(observed['client_exit_code'], 0)

    def test_sigterm_is_handled_and_waits_actual_client(self):
        def interrupt(_child):
            os.kill(os.getpid(), signal.SIGTERM)
        observed, received = self.exercise(interrupt, harness.HarnessInterrupted('expected signal'))
        self.assertEqual(received, [signal.SIGTERM])
        self.assertLess(observed['client_exit_code'], 0)

    def test_reported_stop_error_and_repeated_signal_do_not_mask_primary_or_skip_wait(self):
        primary = RuntimeError('first fixture failure')
        real_stop = harness.stop_leader
        def reported_stop(child):
            real_stop(child)
            os.kill(os.getpid(), signal.SIGTERM)
            os.kill(os.getpid(), signal.SIGTERM)
            raise OSError('reported fixture stop error after actual request')
        harness.stop_leader = reported_stop
        self.addCleanup(setattr, harness, 'stop_leader', real_stop)
        def interrupt(_child):
            raise primary
        observed, received = self.exercise(interrupt, primary)
        self.assertEqual(received, [signal.SIGTERM])
        self.assertIn('reported fixture stop error', observed['cleanup_errors'][0])

    def test_actual_client_exit_does_not_substitute_for_native_exit_report(self):
        observed, _ = self.exercise(lambda _child: None, None, running=False)
        self.assertEqual(observed['client_exit_code'], 0)
        self.assertFalse(harness.native_exit_evidence(None))
        self.assertFalse(harness.native_exit_evidence({'cleanupKnown': True, 'actualSpawns': [observed['client_pid']], 'exits': []}))
        self.assertFalse(harness.native_exit_evidence({'cleanupKnown': True, 'actualSpawns': [observed['client_pid']], 'exits': [{'actualExit': True}]}))

    def test_failed_spawn_has_no_impossible_wait(self):
        with tempfile.TemporaryDirectory(dir=os.environ['TEST_TMPDIR']) as directory:
            observed = {}
            with harness.Interruptions() as interruptions:
                with self.assertRaises(FileNotFoundError):
                    harness._owned_client([str(Path(directory) / 'absent')], {'work': directory, 'env': {'HOME': directory}}, subprocess.PIPE, subprocess.PIPE, interruptions, observed)
            self.assertFalse(observed['client_started'])
            self.assertFalse(observed['client_exit_waited'])
            self.assertEqual(observed['cleanup_errors'], [])


if __name__ == '__main__':
    unittest.main()
