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
                                             self.logs, False, daemons)
        finally:
            for process in daemons:
                cli.stop(process)
        self.assertEqual(len(daemons), 2)
        self.assertTrue(all(process.poll() is not None for process in daemons))
        self.assertIsInstance(result['persisted_revision'], int)


if __name__ == '__main__':
    unittest.main(argv=sys.argv[:1])
