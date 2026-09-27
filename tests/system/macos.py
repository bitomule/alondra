#!/usr/bin/env python3
"""Opt-in macOS demo gate: clone a prepared Tart base, run MAV, retain evidence, delete only this run's clone."""
import argparse
import base64
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import re
import shutil
import signal
import subprocess
import sys
import tarfile
import time
import uuid

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[1]
BASE_NAME = 'alondra-clean-install-20260927'
BASE_ORIGIN = 'ghcr.io/cirruslabs/macos-tahoe-xcode@sha256:49c83cf0989d5c3039b8f1a5c543aa25b2cd920784fdaf30be22e18e4edeaa95'
GUEST = '/Users/admin/alondra-gate'
BASE_MARKER = '/Users/admin/alondra-test-tools/base.json'
BUNDLE = 'com.bitomule.alondra.mac'
MIN_FREE = 8 * 1024**3


class GateError(RuntimeError):
    pass


def checked_name(name):
    if not re.fullmatch(r'alondra-[a-z0-9-]{1,70}', name):
        raise GateError('base must be an explicitly prepared local alondra-* image')
    return name


def run_name():
    return 'alondra-gate-' + uuid.uuid4().hex


def config():
    return '''project_name: Alondra disposable macOS demo
bundle_id: com.bitomule.alondra.mac
process_name: Alondra
target_kind: macos
vm: false
launch:
  mode: custom
  commands:
    build: "false"
    app_path: "printf '%s' /Users/admin/alondra-gate/Alondra.app"
    install: ""
    launch: 'open -n "$MAV_APP_PATH"'
'''


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b''):
            h.update(block)
    return h.hexdigest()


def validate_app(app):
    app = app.resolve(strict=True)
    if app.suffix != '.app' or not app.is_dir():
        raise GateError('--app must name an existing Alondra macOS .app')
    if (app / 'Contents/embedded.provisionprofile').exists():
        raise GateError('use the Debug app without an App Store provisioning profile, not the TestFlight package')
    with (app / 'Contents/Info.plist').open('rb') as stream:
        info = plistlib.load(stream)
    if info.get('CFBundleIdentifier') != BUNDLE or info.get('CFBundleExecutable') != 'Alondra':
        raise GateError('unexpected app identity')
    if info.get('CFBundlePackageType') != 'APPL':
        raise GateError('invalid package type')
    return app, {key: info.get(key) for key in ('CFBundleIdentifier', 'CFBundleShortVersionString', 'CFBundleVersion')}


def extract_evidence(archive, target):
    # Guest artifacts are untrusted. No links, devices or paths outside output.
    with tarfile.open(archive) as source:
        members = source.getmembers()
        total = 0
        for member in members:
            dest = (target / member.name).resolve()
            if not dest.is_relative_to(target.resolve()) or not (member.isfile() or member.isdir()):
                raise GateError('unsafe guest evidence archive')
            total += member.size
            if total > 512 * 1024**2:
                raise GateError('guest evidence exceeds 512 MiB')
        source.extractall(target, members=members, filter='data')


def validate_report(folder):
    reports = list(folder.glob('project/.mav/runs/*/report.json'))
    if len(reports) != 1:
        raise GateError('expected exactly one MAV report')
    report = json.loads(reports[0].read_text())
    expected = {'clean-onboarding', 'demo-open', 'demo-reset', 'demo-exit'}
    steps = report.get('steps') or []
    if {x.get('name') for x in steps} != expected or any(x.get('image', {}).get('ok') is not True for x in steps):
        raise GateError('missing or invalid required demo evidence')
    if report.get('crashes') or report.get('invalid_step_count') != 0 or report.get('video_status') != 'accepted':
        raise GateError('MAV reports crashes, invalid evidence, or missing/invalid video')
    if any(x.get('severity') in ('error', 'blocking') for x in report.get('issues', [])):
        raise GateError('MAV report contains blocking issues')
    run = json.loads((reports[0].parent / 'run.json').read_text())
    if run.get('status') != 'passed' or run.get('skipped'):
        raise GateError('required demo steps were skipped')
    # MAV's semantic parser omits unindexed AXStaticText from text waits. Preserve
    # the actual banner assertion against the driver's raw, recorded AX state.
    banner_seen = False
    for path in reports[0].parent.glob('video/turn-*/app_state.json'):
        if path.stat().st_size > 2 * 1024**2:
            raise GateError('oversized raw AX state')
        tree = json.loads(path.read_text()).get('tree_markdown', '')
        if ('AXStaticText = "Demo · Sample data"' in tree
                and 'AXButton (Reset demo)' in tree and 'AXButton (Exit demo)' in tree):
            banner_seen = True
            break
    if not banner_seen:
        raise GateError('demo banner was not recorded in raw AX evidence')
    return reports[0], report


class Gate:
    def __init__(self, args):
        self.args = args
        self.name = run_name()
        self.out = args.output_dir.resolve()
        self.env = {**os.environ, 'TART_NO_AUTO_PRUNE': 'true'}
        self.process = None
        self.created = False
        self.lock = None
        self.step = 'preflight'
        self.result = {'status': 'incomplete', 'vm': self.name, 'base': args.base,
                       'profile': args.profile,
                       'scope': 'disposable Mac acceptance; individual assertions below, no authenticated service/Claude/APNs/TestFlight installation',
                       'artifact_kind': 'debug-app' if args.profile == 'app' else 'public-brew',
                       'commands': [], 'cleanup': {}}

    def call(self, *args, timeout=60, input_file=None, output_file=None, check=True):
        self.result['commands'].append({'argv': list(args), 'timeout_seconds': timeout})
        r = subprocess.run(args, cwd=REPO, env=self.env, stdin=input_file,
                           stdout=output_file or subprocess.PIPE, stderr=subprocess.PIPE, timeout=timeout)
        if check and r.returncode:
            # Everything sent to the guest is synthetic. Bound operational diagnostics.
            (self.out / 'failure.txt').write_bytes((r.stdout or b'')[-8000:] + r.stderr[-8000:])
            raise GateError(f'{self.step}: command exited {r.returncode}')
        return r

    def guest(self, *args, timeout=60, **kwargs):
        return self.call('tart', 'exec', self.name, *args, timeout=timeout, **kwargs)

    def collect_evidence(self):
        # Short text responses avoid Tart's observed truncation of long binary stdout.
        # Each retry asks for the same immutable range; no append occurs before validation.
        deadline = time.monotonic() + 300
        helper = GUEST + '/evidence-transfer.py'
        attempts = self.result['collection_attempts'] = []
        def request(action, *args):
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise GateError('evidence collection exceeded 300 seconds')
            response = self.guest('/usr/bin/python3', helper, action, *map(str, args),
                                  timeout=min(60, remaining), check=False)
            if response.returncode:
                raise GateError('guest evidence request failed: ' + response.stderr[-500:].decode(errors='replace'))
            if len(response.stdout) > 128 * 1024:
                raise GateError('evidence response exceeds bound')
            return json.loads(response.stdout)
        try:
            diagnostics = request('diagnostics')
            (self.out / 'guest-diagnostics.json').write_text(json.dumps(diagnostics, indent=2) + '\n')
        except (GateError, ValueError, subprocess.TimeoutExpired) as error:
            self.result['diagnostics_error'] = str(error)
        initial = request('prepare')
        size = initial.get('size')
        expected_hash = initial.get('sha256')
        if (initial.get('schema') != 1 or type(size) is not int or not 0 < size <= 544 * 1024**2
                or initial.get('chunk_bytes') != 65536 or not isinstance(expected_hash, str)
                or not re.fullmatch(r'[0-9a-f]{64}', expected_hash)):
            raise GateError('invalid frozen evidence descriptor')
        if shutil.disk_usage(self.out).free < MIN_FREE + size:
            raise GateError('insufficient space for frozen evidence')
        self.result['evidence_descriptor'] = initial
        partial = self.out / 'evidence.partial'
        hasher = hashlib.sha256()
        with partial.open('xb') as stream:
            for offset in range(0, size, 65536):
                for attempt in range(1, 4):
                    detail = {'offset': offset, 'attempt': attempt}
                    try:
                        value = request('chunk', offset)
                        if (value.get('schema') != 1 or value.get('offset') != offset
                                or value.get('total') != size or value.get('archive_sha256') != expected_hash):
                            raise GateError('evidence chunk identity mismatch')
                        data = base64.b64decode(value.get('data', ''), validate=True)
                        if (len(data) != min(65536, size - offset)
                                or hashlib.sha256(data).hexdigest() != value.get('sha256')):
                            raise GateError('evidence chunk size/hash mismatch')
                        detail['verified'] = True
                        attempts.append(detail)
                        break
                    except (GateError, ValueError, TypeError, subprocess.TimeoutExpired) as error:
                        detail['error'] = str(error)
                        attempts.append(detail)
                        if attempt == 3 or time.monotonic() >= deadline:
                            raise GateError('evidence chunk transfer failed; partial retained') from error
                stream.write(data)
                hasher.update(data)
        if hasher.hexdigest() != expected_hash or request('describe') != initial:
            raise GateError('frozen evidence global hash/descriptor mismatch')
        archive = self.out / 'evidence.tar'
        os.link(partial, archive)  # never overwrite an existing completed artifact
        partial.unlink()
        extract_evidence(archive, self.out / 'evidence')

    def execute(self):
        if self.out.exists():
            raise GateError('output directory already exists; choose a new evidence directory')
        self.out.mkdir(parents=True, mode=0o700)
        started = time.monotonic()
        try:
            self.result['source'] = {
                'runner_commit': self.call('git', 'rev-parse', 'HEAD').stdout.decode().strip(),
                'runner_dirty': bool(self.call('git', 'status', '--porcelain').stdout.strip())}
            if self.args.profile == 'app':
                app, identity = validate_app(self.args.app)
                self.result['app'] = identity
                self.result['input_executable_sha256'] = digest(app / 'Contents/MacOS/Alondra')
                self.result['source']['app_source'] = self.args.app_source
                self.call('codesign', '--verify', '--deep', '--strict', str(app))
            tart_home = Path(os.environ.get('TART_HOME', str(Path.home() / '.tart'))).resolve(strict=True)
            if shutil.disk_usage(tart_home).free < MIN_FREE or shutil.disk_usage(self.out).free < MIN_FREE:
                raise GateError('at least 8 GiB free required on Tart and evidence volumes')
            # A local lock serializes this suite across worktrees. External builds still
            # require the repository's one-heavy-job coordination policy.
            self.lock = open(tart_home / '.alondra-gate.lock', 'a')
            fcntl.flock(self.lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
            entries = json.loads(self.call('tart', 'list', '--format', 'json').stdout)
            base = [x for x in entries if x.get('Name') == self.args.base and x.get('Source') == 'local']
            if len(base) != 1 or base[0].get('State') != 'stopped':
                raise GateError('prepared local base must exist and be stopped; no automatic download or personal-image fallback')
            if any(x.get('State') == 'running' for x in entries):
                raise GateError('another VM is running; coordinate the heavy slot before retrying')
            self.result['tart_version'] = self.call('tart', '--version').stdout.decode().strip()
            self.result['base_config_sha256'] = digest(tart_home / 'vms' / self.args.base / 'config.json')
            self.step = 'clone'
            if any(x.get('Name') == self.name for x in entries):
                raise GateError('generated VM name already exists')
            (self.out / 'ownership.json').write_text(json.dumps({
                'vm': self.name, 'base': self.args.base, 'tart_home': str(tart_home),
                'source': self.result['source'], 'state': 'reserved_before_clone'}, indent=2) + '\n')
            self.created = True  # Own this reserved name even if cloning only partially succeeds.
            self.call('tart', 'clone', self.args.base, self.name, timeout=90)
            self.call('tart', 'set', self.name, '--cpu', '2', '--memory', '4096')
            self.step = 'boot'
            with (self.out / 'tart-run.log').open('wb') as log:
                self.process = subprocess.Popen(['tart', 'run', self.name, '--no-graphics', '--no-audio', '--no-clipboard'], env=self.env, stdout=log, stderr=log)
            deadline = time.monotonic() + 180
            while time.monotonic() < deadline:
                if self.process.poll() is not None:
                    raise GateError('VM exited before guest agent became ready')
                try:
                    r = self.guest('/usr/bin/true', timeout=5, check=False)
                    if r.returncode == 0:
                        break
                except subprocess.TimeoutExpired:
                    pass
                time.sleep(2)
            else:
                raise GateError('guest agent unavailable after 180 seconds')
            marker = json.loads(self.guest('/bin/cat', BASE_MARKER).stdout)
            if marker.get('origin') != BASE_ORIGIN or marker.get('synthetic') is not True:
                raise GateError('missing approved synthetic base provenance')
            self.result['base_marker'] = marker
            self.step = 'stage'
            stage = self.out / 'payload'
            stage.mkdir()
            shutil.copyfile(HERE / 'macos/evidence-transfer.py', stage / 'evidence-transfer.py')
            if self.args.profile == 'app':
                shutil.copytree(app, stage / 'Alondra.app', symlinks=True)
                shutil.copyfile(HERE / 'macos/guest.sh', stage / 'guest.sh')
                shutil.copyfile(HERE / 'macos/demo.yaml', stage / 'demo.yaml')
                shutil.copyfile(HERE / 'macos/cua-session.py', stage / 'cua-session.py')
                (stage / 'config.yaml').write_text(config())
            else:
                shutil.copyfile(HERE / 'macos/cli.py', stage / 'cli.py')
                shutil.copyfile(HERE / 'macos/cli.sh', stage / 'guest.sh')
            self.call('/usr/bin/tar', '-cf', str(self.out / 'payload.tar'), '-C', str(stage), '.', timeout=90)
            self.result['payload_sha256'] = digest(self.out / 'payload.tar')
            self.guest('/bin/mkdir', '-p', GUEST)
            with (self.out / 'payload.tar').open('rb') as payload:
                self.call('tart', 'exec', '-i', self.name, '/usr/bin/tar', '-xf', '-', '-C', GUEST, timeout=120, input_file=payload)
            self.step = self.args.profile
            parameter = 'debug' if self.args.profile == 'app' else self.args.brew_version
            try:
                test = self.guest('/bin/bash', GUEST + '/guest.sh', parameter, timeout=600, check=False)
                (self.out / 'guest.txt').write_bytes((test.stdout or b'') + test.stderr)
                self.result['guest_exit_code'] = test.returncode
                if test.returncode:
                    self.result['guest_failure'] = {'exit_code': test.returncode}
            except subprocess.TimeoutExpired as error:
                (self.out / 'guest.txt').write_bytes((error.stdout or b'') + (error.stderr or b''))
                self.result['guest_failure'] = {'timeout_seconds': 600}
                # Stop only this clone's MAV run before freezing partial evidence.
                try:
                    stop = self.guest('/bin/bash', '-c',
                                      'cd /Users/admin/alondra-gate/project && /Users/admin/alondra-test-tools/mav stop',
                                      timeout=30, check=False)
                    self.result['timeout_mav_stop_exit'] = stop.returncode
                except (subprocess.TimeoutExpired, OSError) as stop_error:
                    self.result['timeout_mav_stop_error'] = {
                        'kind': type(stop_error).__name__, 'detail': str(stop_error)}
            self.step = 'collect'
            self.collect_evidence()
            if 'guest_failure' in self.result:
                raise GateError('guest execution failed; see guest_failure and collected diagnostics/evidence')
            if self.args.profile == 'app':
                report_path, report = validate_report(self.out / 'evidence')
                self.result['mav_report'] = str(report_path.relative_to(self.out))
                self.result['mav_report_sha256'] = digest(report_path)
            else:
                cli = json.loads((self.out / 'evidence/project/cli-result.json').read_text())
                if cli.get('status') != 'passed' or cli.get('version') != self.args.brew_version or cli.get('cleanup') is not True:
                    raise GateError('CLI guest assertions failed or wrong Brew version')
                self.result['cli'] = cli
            self.result['status'] = 'passed'
        except Exception as error:
            self.result.update(status='failed', failure={'step': self.step, 'kind': type(error).__name__, 'detail': str(error)})
        finally:
            for sig in (signal.SIGINT, signal.SIGTERM):
                signal.signal(sig, signal.SIG_IGN)
            self.cleanup()
            self.result['elapsed_seconds'] = round(time.monotonic() - started, 3)
            (self.out / 'result.json').write_text(json.dumps(self.result, indent=2) + '\n')
        return self.result

    def cleanup(self):
        if self.created:
            try:
                self.call('tart', 'stop', self.name, timeout=40, check=False)
                if self.process:
                    self.process.wait(timeout=20)
                self.call('tart', 'delete', self.name, timeout=40)
                self.result['cleanup']['owned_clone_deleted'] = True
            except Exception as error:
                self.result['cleanup'] = {'owned_clone_deleted': False, 'detail': str(error), 'remaining_vm': self.name}
                self.result['status'] = 'failed'
        if self.lock:
            self.lock.close()


def main():
    p = argparse.ArgumentParser(description=__doc__)
    p.add_argument('--run', action='store_true', help='without this flag, print the plan and start nothing')
    p.add_argument('--base', default=BASE_NAME, type=checked_name)
    p.add_argument('--profile', choices=('cli', 'app'), default='cli')
    p.add_argument('--brew-version', default='0.1.8', help='assert this exact public Brew version')
    p.add_argument('--app', type=Path)
    p.add_argument('--app-source', help='exact source SHA that produced the input app (recorded, not inferred from runner)')
    p.add_argument('--output-dir', required=True, type=Path)
    args = p.parse_args()
    if args.profile == 'app' and (not args.app or not re.fullmatch('[0-9a-f]{40}', args.app_source or '')):
        p.error('app profile requires --app and full --app-source commit SHA')
    if args.profile == 'cli' and (args.app or args.app_source):
        p.error('Apple artifact/signature flags require --profile app')
    if not re.fullmatch(r'[0-9]+\.[0-9]+\.[0-9]+', args.brew_version):
        p.error('--brew-version must be a numeric release version')
    if not args.run:
        print(json.dumps({'status': 'planned', 'base': args.base, 'profile': args.profile, 'app': str(args.app), 'output': str(args.output_dir), 'starts_vm': False}))
        return 0
    def interrupted(signum, frame):
        raise GateError(f'interrupted by signal {signum}')
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    result = Gate(args).execute()
    print(json.dumps({'status': result['status'], 'result': str(args.output_dir.resolve() / 'result.json'), 'cleanup': result['cleanup']}))
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    try:
        raise SystemExit(main())
    except (GateError, OSError) as error:
        print(str(error), file=sys.stderr)
        raise SystemExit(2)
