#!/usr/bin/env python3
"""Bounded native visual scenarios in the existing Gate-owned Tart clone only."""
import hashlib
import json
import math
import os
from pathlib import Path
import plistlib
import re
import resource
import signal
import stat
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


def structured(value):
    if not isinstance(value, dict) or value.get('isError') is True or value.get('refusal'):
        raise ValueError('invalid Cua response')
    value = value.get('structuredContent', value)
    if not isinstance(value, dict): raise ValueError('invalid Cua structured response')
    return value


def select_window(value, pid):
    windows = structured(value).get('windows')
    if not isinstance(windows, list): raise ValueError('missing Cua windows')
    candidates = []
    for window in windows:
        if not isinstance(window, dict) or window.get('pid') != pid or window.get('is_on_screen') is not True:
            continue
        identity, bounds = window.get('window_id'), window.get('bounds', {})
        width, height = bounds.get('width'), bounds.get('height')
        if (type(identity) is not int or identity <= 0 or type(width) not in (int, float)
                or type(height) not in (int, float) or not math.isfinite(width * height)
                or width <= 0 or height <= 0):
            raise ValueError('invalid Cua window geometry')
        candidates.append((width * height, identity))
    candidates.sort(reverse=True)
    if not candidates or (len(candidates) > 1 and candidates[0][0] == candidates[1][0]):
        raise ValueError('missing or ambiguous owned window')
    return candidates[0][1]


def validate_tree(value, pid, window):
    value = structured(value)
    if value.get('pid') != pid or value.get('window_id') != window:
        raise ValueError('Cua tree is not the owned window')
    tree, snapshot = value.get('tree_markdown'), value.get('snapshot_id')
    if (not isinstance(tree, str) or not tree.strip() or not isinstance(snapshot, str) or not snapshot
            or value.get('degraded') is True or value.get('degraded_reason')
            or value.get('truncated') is True or value.get('nodes_pending', 0) != 0
            or 'AX tree truncated' in tree
            or any((len(line) - len(line.lstrip(' '))) // 2 >= 40 for line in tree.splitlines())):
        raise ValueError('missing or degraded full Cua tree')
    return tree, snapshot


class OwnedTreeReader:
    """Observe the exact live child through the public Cua CLI, preserving full AX."""
    def __init__(self, process, directory, env, run_id):
        self.process, self.directory, self.env, self.run_id = process, directory, env, run_id
        self.count = 0
        self.total = 0
        self.directory.mkdir(mode=0o700)

    def alive(self):
        if self.process.poll() is not None: raise ValueError('owned application exited before AX read')

    def call(self, tool, arguments, closing=False):
        if not closing: self.alive()
        self.count += 1
        if self.count > (121 if closing else 120): raise ValueError('full AX request budget exceeded')
        path = self.directory / ('%03d-%s.json' % (self.count, tool))
        errors = path.with_suffix('.stderr')
        with path.open('xb') as output, errors.open('xb') as stderr:
            command = subprocess.Popen(['/Applications/CuaDriver.app/Contents/MacOS/cua-driver', 'call', tool,
                json.dumps({**arguments, 'session': 'alondra-visual-' + self.run_id})], env=self.env,
                stdout=output, stderr=stderr, start_new_session=True, preexec_fn=bounded_log)
            try:
                command.wait(timeout=10)
            finally:
                stop_owned(command)
        if not closing: self.alive()
        self.total += path.stat().st_size + errors.stat().st_size
        if command.returncode or path.stat().st_size > 4 * 1024**2 or self.total > 32 * 1024**2:
            raise ValueError('full AX command failed or evidence exceeded bounds')
        return json.loads(path.read_text()), path

    def __call__(self):
        windows, _ = self.call('list_windows', {'pid': self.process.pid})
        window = select_window(windows, self.process.pid)
        result, path = self.call('get_window_state', {'pid': self.process.pid, 'window_id': window,
                                                     'max_elements': 2000, 'max_depth': 40, 'include_screenshot': False})
        tree, snapshot = validate_tree(result, self.process.pid, window)
        driver.atomic_json(path.with_suffix('.receipt.json'), {'runID': self.run_id,
            'pid': self.process.pid, 'windowID': window, 'snapshotID': snapshot,
            'responseSHA256': hashlib.sha256(path.read_bytes()).hexdigest(),
            'walkLimits': {'max_elements': 2000, 'max_depth': 40},
            'coverage': 'Observed AX text; absence is limited to this bounded accessibility walk'})
        return tree

    def close(self):
        self.call('end_session', {}, closing=True)


def release_completed_run(project, assertions):
    """Remove only our stopped run pointer; retain every evidence artifact."""
    receipt = json.loads((assertions / 'driver.json').read_text())
    run = receipt.get('mavRunID', '')
    if receipt.get('status') != 'passed' or not re.fullmatch(r'[A-Za-z0-9_-]{1,80}', run):
        raise ValueError('cannot release an unconfirmed MAV run')
    pointer = project / '.mav/current-run'
    if pointer.is_symlink() or not pointer.is_file() or pointer.stat().st_size > 100:
        raise ValueError('invalid MAV run pointer')
    info = pointer.stat()
    if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o022:
        raise ValueError('MAV pointer is not privately controlled by this guest')
    if pointer.read_text() not in (run, run + '\n'):
        raise ValueError('MAV pointer no longer belongs to the completed scenario')
    pointer.unlink()


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
            tree_reader = None
            failure = None
            with (directory / 'app.log').open('wb') as log:
                try:
                    process = subprocess.Popen([str(executable), '-AppleLanguages', '(en)', '-AppleLocale', 'en_US'],
                                               env=env, cwd=directory, stdout=log, stderr=subprocess.STDOUT,
                                               start_new_session=True, preexec_fn=bounded_log)
                    driver.atomic_json(directory / 'ownership.json', {'pid': process.pid, 'runID': run, 'executable': str(executable)})
                    tree_reader = OwnedTreeReader(process, directory / 'full-ax', env, run)
                    driver.exercise(mav, read, lambda value: driver.atomic_json(directory / 'visual-control.json', value),
                                    directory / 'assertions', run, scenario, project=PROJECT,
                                    read_tree=tree_reader)
                    if process.poll() is not None:
                        raise RuntimeError('visual application exited before owned cleanup')
                except BaseException as error:
                    failure = error
                    raise
                finally:
                    try:
                        if tree_reader is not None:
                            try:
                                tree_reader.close()
                            except Exception as error:
                                driver.atomic_json(directory / 'ax-cleanup-error.json', {'error': type(error).__name__})
                                if failure is None: raise
                    finally:
                        if process is not None: stop_owned(process)
            release_completed_run(PROJECT, directory / 'assertions')
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
