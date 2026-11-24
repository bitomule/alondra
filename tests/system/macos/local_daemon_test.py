#!/usr/bin/env python3
"""The CLI gate's local daemon checks, against the alondra and alondrad of this checkout.

The Tart gate runs the same checks on the published package; here they run on every
build, so a route those checks rely on cannot be retired without this test failing.
"""
from pathlib import Path
import re
import shutil
import socket
import sys
import tempfile
import unittest
from unittest import mock

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cli  # noqa: E402

BINARY, DAEMON = (Path(arg).resolve() for arg in sys.argv[1:3])


class LocalDaemonTest(unittest.TestCase):
    def setUp(self):
        root = Path(tempfile.mkdtemp(prefix='local-daemon-', dir=Path(tempfile.gettempdir()).resolve()))
        self.addCleanup(shutil.rmtree, root)
        self.home = root / 'home'
        self.state = self.home / '.alondra'
        self.logs = root / 'logs'
        for directory in (self.home, self.home / '.claude', self.logs):
            directory.mkdir(mode=0o700)
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0))
            self.port = reservation.getsockname()[1]
        self.env = {'PATH': '/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(self.home), 'USER': 'alondra-test',
                    'LOGNAME': 'alondra-test', 'ALONDRA_LANG': 'en', 'TMPDIR': str(root),
                    'ALONDRA_HOME': str(self.state), 'CLAUDE_HOME': str(self.home / '.claude'),
                    'ALONDRA_PORT': str(self.port), 'ALONDRA_LISTEN': '127.0.0.1', 'ALONDRA_WAKE': '0'}

    def test_empty_home_answers_and_survives_a_restart_unchanged(self):
        version = re.match(r'alondra (\S+)', cli.command([str(BINARY), '--version'], self.env, self.logs / 'version.log'))
        self.assertIsNotNone(version)
        daemons = []
        try:
            result = cli.local_daemon_checks(BINARY, DAEMON, self.env, self.port, self.state, version.group(1),
                                             self.logs, False, daemons, ready_within=None)
        finally:
            for process in daemons:
                cli.stop(process)
        self.assertEqual(len(daemons), 2)
        self.assertTrue(all(process.poll() is not None for process in daemons))
        self.assertIsInstance(result['persisted_revision'], int)


class ReadinessTest(unittest.TestCase):
    """The readiness wait ends on the daemon's answer or exit, never on a budget of its own: a
    disk that makes the first start slow must not fail the test. The clock is fake, so the wait
    that outlasts any budget costs nothing."""

    class Clock:
        def __init__(self):
            self.now = 0.0

        def monotonic(self):
            return self.now

        def sleep(self, seconds):
            self.now += seconds

    def wait(self, process, answers_at, version='1.0.0'):
        clock = self.Clock()
        process.clock = clock

        def request(port, path, timeout=2):
            if clock.now < answers_at:
                raise ConnectionRefusedError()
            return {'version': version}

        with mock.patch.object(cli, 'time', clock), mock.patch.object(cli, 'request', request):
            return cli.readiness(process, 1, '1.0.0'), clock.now

    def test_waits_for_an_answer_that_comes_after_any_budget(self):
        answer, waited = self.wait(self.process(), answers_at=600)
        self.assertEqual(answer['version'], '1.0.0')
        self.assertGreaterEqual(waited, 600)

    def test_fails_when_the_daemon_exits_before_answering(self):
        with self.assertRaisesRegex(RuntimeError, 'daemon exited before readiness'):
            self.wait(self.process(exits_at=5), answers_at=600)

    def test_fails_when_the_daemon_answers_another_version(self):
        with self.assertRaisesRegex(RuntimeError, 'daemon version mismatch'):
            self.wait(self.process(), answers_at=0, version='0.0.1')

    def process(self, exits_at=None):
        class Daemon:
            clock = None

            def poll(self):
                return None if exits_at is None or self.clock.now < exits_at else 1

        return Daemon()


if __name__ == '__main__':
    unittest.main(argv=sys.argv[:1])
