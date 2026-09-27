#!/usr/bin/env python3
"""Keep MAV's two CLI calls in the same public driver session, inside this VM only."""
import json
import os
from pathlib import Path
import subprocess
import sys

REAL = '/Applications/CuaDriver.app/Contents/MacOS/cua-driver'
args = sys.argv[1:]
if args[:1] == ['call'] and len(args) == 3 and args[1] in ('get_window_state', 'click'):
    if not subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.model'], text=True).startswith('VirtualMac'):
        raise SystemExit('This adapter is restricted to the disposable VM')
    if not Path('/Users/admin/alondra-test-tools/base.json').is_file():
        raise SystemExit('Missing synthetic base marker')
    payload = json.loads(args[2])
    payload['session'] = 'alondra-mav-demo'
    args[2] = json.dumps(payload)
os.execv(REAL, [REAL, *args])
