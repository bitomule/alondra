#!/usr/bin/env python3
"""Keep MAV's two CLI calls in the same public driver session, inside this VM only."""
import json
import os
from pathlib import Path
import subprocess
import sys
import stat
import shutil

REAL = '/Applications/CuaDriver.app/Contents/MacOS/cua-driver'
def preserve_ax(project):
    """Copy driver evidence before MAV deletes its rendering inputs on video.stop."""
    entries = []
    total = 0
    for run in (project / '.mav/runs').glob('*'):
        if run.is_symlink() or not run.is_dir():
            raise ValueError('invalid evidence run')
        for turn in (run / 'video').glob('turn-*'):
            if turn.is_symlink() or not turn.is_dir():
                raise ValueError('invalid evidence turn')
            for name in ('action.json', 'before_state.json', 'after_state.json', 'app_state.json'):
                source = turn / name
                if not source.exists(): continue
                info = source.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_size > 2 * 1024**2:
                    raise ValueError('invalid AX evidence file')
                entries.append((source, project / 'driver-ax' / run.name / turn.name / name))
                total += info.st_size
                if len(entries) > 2000 or total > 32 * 1024**2:
                    raise ValueError('AX evidence exceeds bound')
    for source, target in entries:
        for parent in (target.parent, *target.parents):
            if parent == project: break
            if parent.is_symlink(): raise ValueError('AX evidence parent is symlink')
        target.parent.mkdir(parents=True, exist_ok=True)
        if target.is_symlink(): raise ValueError('AX evidence destination is symlink')
        shutil.copyfile(source, target)


def main():
    args = sys.argv[1:]
    tracked = args[:1] == ['call'] and len(args) == 3 and args[1] in ('get_window_state', 'click')
    if tracked:
        if not subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.model'], text=True).startswith('VirtualMac'):
            raise SystemExit('This adapter is restricted to the disposable VM')
        if not Path('/Users/admin/alondra-test-tools/base.json').is_file():
            raise SystemExit('Missing synthetic base marker')
        payload = json.loads(args[2])
        payload['session'] = 'alondra-mav-demo'
        args[2] = json.dumps(payload)
    result = subprocess.run([REAL, *args])  # inherit exact stdout/stderr, preserve status
    if tracked:
        try:
            preserve_ax(Path('/Users/admin/alondra-gate/project'))
        except Exception as error:
            print('AX evidence preservation failed: ' + type(error).__name__, file=sys.stderr)
    return result.returncode


if __name__ == '__main__':
    raise SystemExit(main())
