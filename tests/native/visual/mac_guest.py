#!/usr/bin/env python3
"""Bounded native visual scenarios in the existing Gate-owned Tart clone only."""
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import resource
import signal
import sys
import subprocess
import uuid

import driver

ROOT = Path('/Users/admin/alondra-gate')
PROJECT = ROOT / 'project'
BUNDLE = 'com.bitomule.alondra.visualharness'


def bounded_log():
    resource.setrlimit(resource.RLIMIT_FSIZE, (4 * 1024**2, 4 * 1024**2))


def stop_owned(process):
    if process.poll() is None:
        os.killpg(process.pid, signal.SIGTERM)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            os.killpg(process.pid, signal.SIGKILL)
            process.wait(timeout=5)


def prepare():
    os.umask(0o077)
    if not subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.model'], text=True).startswith('VirtualMac'):
        raise RuntimeError('visual runtime is restricted to the disposable VM')
    marker = json.loads(Path('/Users/admin/alondra-test-tools/base.json').read_text())
    if marker.get('synthetic') is not True: raise RuntimeError('non-synthetic base')
    app = ROOT / 'Alondra.app'
    info = plistlib.loads((app / 'Contents/Info.plist').read_bytes())
    name = info.get('CFBundleExecutable', '')
    if info.get('CFBundleIdentifier') != BUNDLE or not re.fullmatch(r'[A-Za-z0-9_-]+', name):
        raise RuntimeError('unexpected visual app')
    executable = app / 'Contents/MacOS' / name
    if executable.is_symlink() or not executable.is_file(): raise RuntimeError('invalid visual executable')
    config = {'project_name': 'Alondra synthetic visual scenarios', 'bundle_id': BUNDLE, 'process_name': name,
              'target_kind': 'macos', 'vm': False, 'launch': {'mode': 'custom', 'commands': {
                  'build': 'false', 'app_path': "printf '%s' /Users/admin/alondra-gate/Alondra.app", 'install': '', 'launch': ''}}}
    (PROJECT / '.mav/config.yaml').write_text(json.dumps(config))
    return executable


def main():
    executable = prepare()
    summary = {'schema': 1, 'status': 'running', 'cleanup': False, 'scenarios': [],
               'binarySHA256': hashlib.sha256(executable.read_bytes()).hexdigest()}
    path = PROJECT / 'visual-summary.json'
    driver.atomic_json(path, summary)
    try:
        for scenario in driver.CHECKS:
            run = str(uuid.uuid4())
            directory = PROJECT / scenario
            directory.mkdir(mode=0o700)
            env = {key: os.environ[key] for key in ('PATH', 'HOME', 'TMPDIR', 'LANG') if key in os.environ}
            env.update(ALONDRA_VISUAL_RUN_ID=run, ALONDRA_VISUAL_SCENARIO=scenario,
                       ALONDRA_VISUAL_DIRECTORY=str(directory), MAV_TARGET_KIND='macos')
            command_count = 0
            def mav(*args):
                nonlocal command_count
                command_count += 1
                log = directory / ('command-%03d.log' % command_count)
                with log.open('wb') as stream:
                    command = subprocess.Popen(['mav', *args], cwd=PROJECT, env=env, stdout=stream,
                                               stderr=subprocess.STDOUT, start_new_session=True, preexec_fn=bounded_log)
                    try:
                        command.wait(timeout=60)
                    finally:
                        stop_owned(command)
                if command.returncode: raise RuntimeError('MAV command failed; see ' + log.name)
                return log.read_text()
            def read():
                result = directory / 'visual-result.json'
                if not result.exists(): return None
                if result.is_symlink() or result.stat().st_size > 1024**2: raise ValueError('invalid visual receipt')
                return json.loads(result.read_text())
            process = None
            with (directory / 'app.log').open('wb') as log:
                try:
                    process = subprocess.Popen([str(executable), '-AppleLanguages', '(en)', '-AppleLocale', 'en_US'],
                                               env=env, cwd=directory, stdout=log, stderr=subprocess.STDOUT,
                                               start_new_session=True, preexec_fn=bounded_log)
                    driver.atomic_json(directory / 'ownership.json', {'pid': process.pid, 'runID': run, 'executable': str(executable)})
                    driver.exercise(mav, read, lambda value: driver.atomic_json(directory / 'visual-control.json', value),
                                    directory / 'assertions', run, scenario, project=PROJECT)
                    if process.poll() is not None:
                        raise RuntimeError('visual application exited before owned cleanup')
                finally:
                    if process is not None: stop_owned(process)
            summary['scenarios'].append({'scenario': scenario, 'runID': run})
            driver.atomic_json(path, summary)
        summary.update(status='passed', cleanup=True)
    except BaseException as error:
        summary.update(status='failed', error=type(error).__name__)
        raise
    finally:
        driver.atomic_json(path, summary)
    return 0


if __name__ == '__main__':
    if sys.argv[1:] == ['--prepare']:
        prepare()
    elif sys.argv[1:]:
        raise SystemExit('usage: mac_guest.py [--prepare]')
    else:
        raise SystemExit(main())
