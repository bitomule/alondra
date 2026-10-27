#!/usr/bin/env python3
"""Upgrade rehearsal: the previous release must replace itself with this one on copies of the live HOME.

The live HOME is only read. Every process the rehearsal starts runs under sandbox-exec, which
refuses writes anywhere in the user's home directory, outbound connections other than loopback,
unix sockets outside the rehearsal's own directory and signals to processes outside the
sandbox. The copies lose the files that reach relay, APNs or a paired device, and every daemon
runs with its own label, port, HOME and TMPDIR and a PATH whose tools only record their calls.
"""
import argparse
import base64
import filecmp
import json
import os
import pathlib
import re
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tarfile
import tempfile
import time
import urllib.request

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent))
import release  # noqa: E402

SCRIPT = pathlib.Path(__file__).resolve()
TEAM = '658YBSK6MG'
LABEL = 'com.alondra.upgrade-rehearsal'
REAL_PORTS = frozenset({47800, 47810})
OUTWARD_FILES = ('service.toml', 'service.toml.staged', 'apns.json')
STUBBED_COMMANDS = ('claude', 'gh', 'git', 'tmux', 'brew', 'launchctl', 'osascript', 'open', 'curl',
                    'terminal-notifier', 'ssh')
SQLITE_HEADER = b'SQLite format 3\0'
SIDECARS = ('-wal', '-shm', '-journal')
LAUNCHER = 'alondrad-launcher'
RELAUNCH = 75
SERVE_WITHIN = 60
EXIT_WITHIN = 120
STAY_UP = 10
PREFLIGHT_TIMEOUT = 300
CASES = ('live', 'empty-legacy-stores', 'tableless-legacy-store', 'outdated-launcher', 'fresh-previous')


class Failed(RuntimeError):
    pass


def profile(protected, work):
    quoted = lambda path: json.dumps(str(path))
    return '\n'.join([
        '(version 1)',
        '(allow default)',
        f'(deny file-write* (subpath {quoted(protected)}))',
        '(deny network-outbound)',
        '(allow network-outbound (remote ip "localhost:*"))',
        f'(allow network-outbound (remote unix-socket (subpath {quoted(work)})))',
        '(deny signal)',
        '(allow signal (target same-sandbox))',
    ])


def sandboxed(argv, protected, work):
    if inside(work, protected):
        raise Failed(f'The rehearsal directory {work} must be outside {protected}')
    return ['/usr/bin/sandbox-exec', '-p', profile(protected, work), *map(str, argv)]


def inside(path, root):
    path, root = pathlib.Path(path).resolve(), pathlib.Path(root).resolve()
    return path == root or root in path.parents


def version_key(ver):
    return tuple(int(part) for part in ver.split('.'))


def previous_version(new, tags):
    """`latest` is for a development build, whose version is not the one it will ship as."""
    versions = [tag.removeprefix('alondra-v') for tag in tags
                if re.fullmatch(r'alondra-v\d+\.\d+\.\d+', tag)]
    older = [ver for ver in versions if new == 'latest' or version_key(ver) < version_key(new)]
    if not older:
        raise Failed(f'No published release older than {new} to upgrade from')
    return max(older, key=version_key)


def published_tags():
    return [item['tag_name'] for item in release.pages(f'repos/{release.REPO}/releases')
            if item.get('draft') is False and isinstance(item.get('tag_name'), str)]


def formula_sha(ver):
    page = 1
    while commits := release.api(f'repos/{release.REPO}/commits?path=Formula/alondra.rb&per_page=100&page={page}'):
        for commit in commits:
            content = release.api(f'repos/{release.REPO}/contents/Formula/alondra.rb?ref={commit["sha"]}')
            text = base64.b64decode(content['content']).decode()
            if re.search(rf'^\s*version "{re.escape(ver)}"$', text, re.M):
                found = re.search(r'^\s*sha256 "([0-9a-f]{64})"$', text, re.M)
                if found:
                    return found.group(1)
        page += 1
    raise Failed(f'No published formula installs {ver}')


def fetch_previous(ver, into):
    tag = f'alondra-v{ver}'
    found = release.lookup_release(tag)
    if found is None or found['draft']:
        raise Failed(f'{tag} is not a published release')
    name = f'alondra-{ver}-{release.TARGETS[0]}.tar.gz'
    asset = release.release_assets(found['id']).get(name)
    if asset is None:
        raise Failed(f'{tag} has no {name}')
    archive = into / name
    release.download_asset(asset['id'], archive)
    digest = release.sha(archive)
    expected = {asset.get('digest', '').removeprefix('sha256:'), formula_sha(ver)}
    if expected != {digest}:
        raise Failed(f'{name} does not match its published checksum')
    return archive


def unpack(archive, into, signed=True, published=False):
    """A release already published may predate the launcher; the one about to ship may not."""
    allowed = release.PUBLISHED_FILES if published else (release.PUBLIC_FILES,)
    release.validate_archive(archive, allowed)
    into.mkdir(parents=True)
    with tarfile.open(archive) as tar:
        names = [name for name in ('alondrad', LAUNCHER) if name in tar.getnames()]
        for name in names:
            (into / name).write_bytes(tar.extractfile(name).read())
            (into / name).chmod(tar.getmember(name).mode & 0o755)
    if signed:
        for name in names:
            subprocess.run(['codesign', '--verify', '--strict', into / name], check=True)
            details = subprocess.run(['codesign', '--display', '--verbose=4', into / name],
                                     check=True, text=True, capture_output=True).stderr
            if f'TeamIdentifier={TEAM}' not in details:
                raise Failed(f'{name} in {archive.name} is not signed by the release team')
    return into / 'alondrad'


def binary_version(binary):
    out = subprocess.run([binary, '--version'], check=True, text=True, capture_output=True).stdout
    return out.split()[-1]


def needs_sqlite_copy(path):
    """A WAL database with no -wal or -shm beside it was closed cleanly and nothing writes it, so
    its bytes are consistent; SQLite could not open it read-only anyway without creating -shm."""
    if not path.is_file() or path.is_symlink() or not path.stat().st_size:
        return False
    with path.open('rb') as file:
        header = file.read(20)
    if not header.startswith(SQLITE_HEADER):
        return False
    wal_mode = len(header) == 20 and header[18] == 2
    return not wal_mode or any(path.with_name(path.name + suffix).exists() for suffix in ('-wal', '-shm'))


def clone(sources, target):
    """APFS clones keep modes and cost no space; a socket is a live endpoint, never copied."""
    sources = [str(path) for path in sources if not path.is_socket()]
    if not sources:
        return
    result = subprocess.run(['/bin/cp', '-c', '-R', '-p', *sources, str(target)], capture_output=True, text=True)
    if result.returncode:
        raise Failed(f'cloning into {target}: {last_line(result.stderr)}')


def copy_home(source, target):
    """Run inside the sandbox: SQLite through read-only connections, the rest as APFS clones."""
    source, target = source.resolve(), target.resolve()
    target.mkdir(mode=0o700)
    entries = [entry for entry in sorted(source.iterdir())
               if not entry.is_socket() and entry.suffix != '.log' and not entry.name.endswith(SIDECARS)]
    databases = [entry for entry in entries if needs_sqlite_copy(entry)]
    clones = [entry for entry in entries if entry not in databases]
    if clones:
        clone(clones, target)
    for database in databases:
        reader = sqlite3.connect(f'{database.as_uri()}?mode=ro', uri=True)
        writer = sqlite3.connect(target / database.name)
        try:
            reader.backup(writer)
        finally:
            writer.close()
            reader.close()
        os.chmod(target / database.name, database.stat().st_mode & 0o777)
    for name in OUTWARD_FILES:
        (target / name).unlink(missing_ok=True)


def digest(home):
    """A read-only SQLite reader may create -shm and an empty -wal; any other change is a write."""
    files = {}
    for path in sorted(home.rglob('*')):
        meta = path.lstat()
        if path.name.endswith('-shm') or (path.name.endswith('-wal') and not meta.st_size):
            continue
        files[str(path.relative_to(home))] = (meta.st_mode, meta.st_size, meta.st_mtime_ns, meta.st_ino)
    return files


def empty_legacy_stores(home):
    for name in ('alondra.db', 'schedules.db'):
        for suffix in ('', *SIDECARS):
            (home / f'{name}{suffix}').unlink(missing_ok=True)
        (home / name).touch(mode=0o600)


def tableless_legacy_store(home):
    for suffix in ('', *SIDECARS):
        (home / f'alondra.db{suffix}').unlink(missing_ok=True)
    connection = sqlite3.connect(home / 'alondra.db')
    connection.execute('VACUUM')
    connection.close()
    os.chmod(home / 'alondra.db', 0o600)


def outdated_launcher(home):
    launcher = home / 'bin' / LAUNCHER
    launcher.parent.mkdir(exist_ok=True)
    launcher.unlink(missing_ok=True)
    launcher.write_text(f"#!/bin/sh\necho '{LAUNCHER} 0'\n")
    launcher.chmod(0o755)


MUTATIONS = {'empty-legacy-stores': empty_legacy_stores, 'tableless-legacy-store': tableless_legacy_store,
             'outdated-launcher': outdated_launcher}


def free_port():
    while True:
        with socket.socket() as probe:
            probe.bind(('127.0.0.1', 0))
            port = probe.getsockname()[1]
        if port not in REAL_PORTS:
            return port


class Rehearsal:
    def __init__(self, work, protected, live_home):
        self.work, self.protected, self.live_home = work, protected, live_home
        for directory in ('fakehome', 'tmp', 'stubs'):
            (work / directory).mkdir()
        for name in STUBBED_COMMANDS:
            stub = work / 'stubs' / name
            stub.write_text(f'#!/bin/sh\nprintf "%s\\n" "{name} $*" >> "{work}/stub-calls.log"\nexit 0\n')
            stub.chmod(0o755)

    def env(self, home, port=None):
        if inside(home, self.protected) and (port is not None or home.resolve() != self.live_home):
            raise Failed(f'Refusing to start a daemon on {home}: only copies outside {self.protected} are used')
        if port in REAL_PORTS:
            raise Failed(f'Port {port} belongs to the real alondrad')
        env = {
            'HOME': str(self.work / 'fakehome'),
            'TMPDIR': str(self.work / 'tmp'),
            'PATH': f'{self.work / "stubs"}:/usr/bin:/bin:/usr/sbin:/sbin',
            'ALONDRA_HOME': str(home),
            'ALONDRA_SERVICE_LABEL': LABEL,
            'ALONDRA_UPDATE_URL': 'http://127.0.0.1:9/',
            'ALONDRA_AUTO_UPDATE': '0',
            'ALONDRA_WAKE': '0',
            'ALONDRA_RESUME_DEAD_NODES': '0',
            'ALONDRA_BINARY_CHECK_SECS': '1',
        }
        if 'USER' in os.environ:
            env['USER'] = os.environ['USER']
        if port is not None:
            env.update(ALONDRA_PORT=str(port), ALONDRA_LISTEN='127.0.0.1', XPC_SERVICE_NAME=LABEL)
        return env

    def run(self, argv, env, timeout):
        return subprocess.run(sandboxed(argv, self.protected, self.work), env=env, text=True,
                              stdin=subprocess.DEVNULL, capture_output=True, timeout=timeout)

    def preflight(self, binary, home):
        started = time.monotonic()
        try:
            result = self.run([binary, '--preflight'], self.env(home), PREFLIGHT_TIMEOUT)
        except subprocess.TimeoutExpired:
            raise Failed(f'preflight did not finish within {PREFLIGHT_TIMEOUT} s') from None
        if result.returncode:
            raise Failed(f'preflight rejected the HOME: {last_line(result.stdout + result.stderr)}')
        return round((time.monotonic() - started) * 1000)

    def copy(self, source, target):
        result = self.run([sys.executable, '-I', '-B', SCRIPT, '--copy-home', source, target], os.environ.copy(), 600)
        if result.returncode:
            raise Failed(f'copying {source}: {last_line(result.stderr)}')


class Daemon:
    def __init__(self, rehearsal, launch_path, home, log, port=None):
        self.port = port or free_port()
        self.log = log
        with log.open('ab') as output:
            self.process = subprocess.Popen(
                sandboxed([launch_path], rehearsal.protected, rehearsal.work),
                env=rehearsal.env(home, self.port), stdin=subprocess.DEVNULL,
                stdout=output, stderr=subprocess.STDOUT)

    def status(self):
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{self.port}/daemon', timeout=0.5) as response:
                return json.loads(response.read())
        except (OSError, ValueError):
            return None

    def wait_for_version(self, ver, within):
        deadline = time.monotonic() + within
        while time.monotonic() < deadline:
            if self.process.poll() is not None:
                raise Failed(f'the daemon exited {self.process.returncode}: {last_line(self.log.read_text())}')
            if (self.status() or {}).get('version') == ver:
                return
            time.sleep(0.02)
        raise Failed(f'the daemon did not answer as {ver} within {within} s: {last_line(self.log.read_text())}')

    def stop(self):
        if self.process.poll() is None:
            self.process.send_signal(signal.SIGTERM)
            try:
                self.process.wait(10)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait()


def last_line(text):
    lines = [line.strip() for line in text.splitlines() if line.strip()]
    return lines[-1] if lines else 'no output'


def launcher_version(rehearsal, home, launcher):
    """What the daemon compares: the line `--version` prints, or None for a missing or broken launcher."""
    if not launcher.is_file():
        return None
    try:
        result = rehearsal.run([launcher, '--version'], rehearsal.env(home), 10)
    except (OSError, subprocess.TimeoutExpired):
        return None
    text = result.stdout.strip()
    return text if not result.returncode and text.startswith(LAUNCHER) else None


def launcher_state(rehearsal, home, launcher):
    return launcher_version(rehearsal, home, launcher), launcher.read_bytes() if launcher.is_file() else None


def check_launcher(rehearsal, home, ver, bundled, installed, before):
    """The daemon's rule: the fixed path ends up with the version the release ships, and its file is
    only replaced when that version changes, since the Local Network permission is tied to it."""
    wanted = launcher_version(rehearsal, home, bundled)
    if wanted is None:
        raise Failed(f'the launcher {ver} ships does not answer --version: {bundled}')
    found = launcher_version(rehearsal, home, installed)
    if found != wanted:
        raise Failed(f'{ver} left {found or "no launcher"} at {installed}; it ships {wanted}')
    if before[0] == wanted:
        if installed.read_bytes() != before[1]:
            raise Failed(f'{ver} replaced {installed} although it already held {wanted}')
    elif not filecmp.cmp(bundled, installed, shallow=False):
        raise Failed(f'{ver} put a launcher at {installed} that is not the one it ships')


def link(path, target):
    staged = path.with_name(path.name + '.next')
    staged.unlink(missing_ok=True)
    staged.symlink_to(target, target_is_directory=True)
    os.replace(staged, path)


def upgrade(rehearsal, case, home, old, new, while_running=None):
    """Homebrew's layout: launchd runs opt/alondra/bin/alondrad and brew upgrade repoints opt.
    `while_running` changes the HOME under the running daemon, as a stray tool did before 0.2.54."""
    root = rehearsal.work / case
    kegs = {}
    for ver, binary in ((old['version'], old['binary']), (new['version'], new['binary'])):
        keg = root / 'Cellar/alondra' / ver
        (keg / 'bin').mkdir(parents=True)
        shutil.copy2(binary, keg / 'bin/alondrad')
        if binary.with_name(LAUNCHER).exists():
            shutil.copy2(binary.with_name(LAUNCHER), keg / 'bin' / LAUNCHER)
        kegs[ver] = keg
    opt = root / 'opt/alondra'
    opt.parent.mkdir(parents=True)
    link(opt, kegs[old['version']])
    launch_path = opt / 'bin/alondrad'
    log = root / 'alondrad.log'
    daemon = Daemon(rehearsal, launch_path, home, log)
    try:
        daemon.wait_for_version(old['version'], SERVE_WITHIN)
        if while_running:
            while_running(home)
        link(opt, kegs[new['version']])
        swapped = time.monotonic()
        try:
            code = daemon.process.wait(EXIT_WITHIN)
        except subprocess.TimeoutExpired:
            rejected = (daemon.status() or {}).get('preflight_rejected') or {}
            reason = rejected.get('reason') or last_line(log.read_text())
            raise Failed(f'{old["version"]} did not restart into {new["version"]} within {EXIT_WITHIN} s: {reason}') from None
        if code != RELAUNCH:
            raise Failed(f'{old["version"]} exited {code} instead of {RELAUNCH}: {last_line(log.read_text())}')
        exited_ms = round((time.monotonic() - swapped) * 1000)
    finally:
        daemon.stop()
    bundled = kegs[new['version']] / 'bin' / LAUNCHER
    installed = home / 'bin' / LAUNCHER
    before = launcher_state(rehearsal, home, installed)
    relaunched = Daemon(rehearsal, launch_path, home, log, port=daemon.port)
    try:
        started = time.monotonic()
        relaunched.wait_for_version(new['version'], SERVE_WITHIN)
        answered_ms = round((time.monotonic() - started) * 1000)
        deadline = time.monotonic() + STAY_UP
        while time.monotonic() < deadline:
            if relaunched.process.poll() is not None or (relaunched.status() or {}).get('version') != new['version']:
                raise Failed(f'{new["version"]} stopped answering after the restart: {last_line(log.read_text())}')
            time.sleep(0.5)
    finally:
        relaunched.stop()
    if bundled.exists():
        check_launcher(rehearsal, home, new['version'], bundled, installed, before)
    return {'exited_after_swap_ms': exited_ms, 'answered_ms': answered_ms}


def fresh_previous(rehearsal, home, old):
    home.mkdir(mode=0o700)
    daemon = Daemon(rehearsal, old['binary'], home, rehearsal.work / 'fresh-previous-setup.log')
    try:
        daemon.wait_for_version(old['version'], SERVE_WITHIN)
    finally:
        daemon.stop()


SDK_CASE = 'sdk-handover'
FIRST_SDK_RELEASE = (0, 3, 0)


def has_sdk_backend(version):
    """From 0.3.0 a release runs every node through the Agent SDK: it leaves no claude --bg node
    for the next one to move."""
    return tuple(int(part) for part in version.split('.')[:3]) >= FIRST_SDK_RELEASE
HANDOVER_STEPS = ('planned', 'bg_stopped', 'sdk_admitted', 'done')
STUBBED_FOR_FAKES = tuple(name for name in STUBBED_COMMANDS if name not in ('claude', 'gh', 'git', 'brew'))
PATIENCE = 300
# The tasks the previous release leaves in progress: the node at rest moves with the answer to its
# card, the crashing one too while the daemon dies after each step of its move, the working one
# only once its turn ended and its next input came, the lost one as Alondra brings it back.
RESTING, CRASHING, WORKING, LOST = 't-resting', 't-crashing', 't-working', 't-lost'
SEEDED = {RESTING: 'Name the settings flag', CRASHING: 'Rename the log file', WORKING: 'Write the release notes',
          LOST: 'Migrate the settings file'}
ANSWER = 'Short name'
DURING_TURN = 'Also list the fixed crashes'
AFTER_TURN = 'Add the release date to the notes'
BROUGHT_BACK = 'Alondra restarted'
LATER_REQUEST = 'req-after-the-update'
# Like the native Claude, a session keeps its transcript: the one claude --bg wrote before the update.
TRANSCRIPT = '"the turns this session had under claude --bg"\n'
# Loaded into every SDK host before it starts: while the gate file exists the host waits, so the
# daemon, which admitted the execution and waits for its host, stops right after recording it.
HOST_GATE = '''const fs = require('fs');
const gate = process.env.ALONDRA_REHEARSAL_HOST_GATE;
if (gate && fs.existsSync(gate)) {
  fs.writeFileSync(gate + '.held', String(process.pid));
  const nap = new Int32Array(new SharedArrayBuffer(4));
  while (fs.existsSync(gate)) Atomics.wait(nap, 0, 0, 20);
}
'''
NATIVE = '''#!/usr/bin/python3
"""The native Claude of the rehearsal's SDK backend: an orchestrator batch (--restricted) goes to the
orchestrator fixture, every other session to the persistent one, which records each start and keeps
each session's transcript."""
import os, sys
here = os.path.dirname(os.path.abspath(__file__))
orchestrator = '--restricted' in sys.argv
if orchestrator:
    os.environ['FIXTURE_CAPTURE'] += '.orchestrator'
script = os.path.join(here, 'orchestrator-fake-claude.py' if orchestrator else 'persistent-fake-claude.py')
os.execv('/usr/bin/python3', ['/usr/bin/python3', script] + sys.argv[1:])
'''


class SdkArtifacts:
    """What the case needs besides the release: the native Claude fixtures and the sandbox tool
    whose fakes stand in for Claude Code and GitHub. The host is the one the new release installs."""

    def __init__(self, fixtures, tool):
        self.fixtures, self.tool = (pathlib.Path(path).resolve() for path in (fixtures, tool))
        for path in (self.tool, *(self.fixtures / name for name in FIXTURES)):
            if not path.is_file():
                raise Failed(f'{path} does not exist: build it with Bazel first')


FIXTURES = ('persistent-fake-claude.py', 'orchestrator-fake-claude.py')


def keg_from_archive(archive, keg, published):
    allowed = release.PUBLISHED_FILES if published else (release.PUBLIC_FILES,)
    release.validate_archive(archive, allowed)
    with tarfile.open(archive) as tar:
        for member in (member for member in tar.getmembers() if member.isfile()):
            name = member.name.removeprefix('./')
            target = keg / 'bin' / name if name in ('alondra', 'alondrad', LAUNCHER) else keg / 'libexec/alondra' / name
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_bytes(tar.extractfile(member).read())
            target.chmod(member.mode & 0o755)


def keg_from_build(binary, cli, host, keg):
    (keg / 'bin').mkdir(parents=True)
    shutil.copy2(binary, keg / 'bin/alondrad')
    shutil.copy2(cli, keg / 'bin/alondra')
    if binary.with_name(LAUNCHER).exists():
        shutil.copy2(binary.with_name(LAUNCHER), keg / 'bin' / LAUNCHER)
    release.stage_assets(release.ROOT, keg / 'libexec/alondra')
    release.stage_host(host, host.with_name('alondra-host.manifest.json'), keg / 'libexec/alondra')


def checksum(path):
    return release.sha(path)


def rows(home, table):
    connection = sqlite3.connect(f'{(home / "domain.db").as_uri()}?mode=ro', uri=True)
    try:
        cursor = connection.execute(f'SELECT * FROM {table} ORDER BY 1')
        names = [column[0] for column in cursor.description]
        return [dict(zip(names, row)) for row in cursor.fetchall()]
    finally:
        connection.close()


def domain_snapshot(home):
    """Every task, request and card as domain.db holds them."""
    return {table: rows(home, table) for table in ('tasks', 'requests', 'interactions')}


def snapshot_changes(before, after):
    changes = []
    for table in sorted(before.keys() | after.keys()):
        old = {row['id']: row for row in before.get(table, [])}
        new = {row['id']: row for row in after.get(table, [])}
        for key in sorted(old.keys() | new.keys()):
            if old.get(key) != new.get(key):
                fields = sorted(name for name in (old.get(key) or {}).keys() | (new.get(key) or {}).keys()
                                if (old.get(key) or {}).get(name) != (new.get(key) or {}).get(name))
                changes.append(f'{table} {key}: {", ".join(fields)}')
    return changes


def journal(home):
    """Each node's move to the SDK, its steps in order."""
    moves = {}
    try:
        for row in rows(home, 'sdk_handover_steps'):
            moves.setdefault(row['node'], []).append(row['step'])
    except sqlite3.OperationalError:
        return {}
    for steps in moves.values():
        steps.sort(key=HANDOVER_STEPS.index)
    return moves


def read_lines(path):
    if not path.exists():
        return []
    return [json.loads(line) for line in path.read_text().splitlines() if line.strip()]


def alive(pid):
    try:
        os.kill(pid, 0)
    except ProcessLookupError:
        return False
    except PermissionError:
        return True
    return True


def until(what, probe, within=PATIENCE):
    """Waits for a fact another process records; on time out, says what it saw last."""
    deadline = time.monotonic() + within
    while True:
        found, seen = probe()
        if found:
            return found
        if time.monotonic() > deadline:
            raise Failed(f'{what}: not within {within} s; last seen: {seen}')
        time.sleep(0.2)


class Supervisor:
    """One sandbox instance for a whole case: macOS lets a sandboxed process signal only processes of
    its own sandbox instance, and the new daemon must stop the claude --bg nodes the previous one
    started. Every process of the case starts here; this script, outside, only kills them."""

    def __init__(self, rehearsal):
        self.process = subprocess.Popen(
            sandboxed([sys.executable, '-I', '-B', SCRIPT, '--supervise'], rehearsal.protected, rehearsal.work),
            env=os.environ.copy(), stdin=subprocess.PIPE, stdout=subprocess.PIPE, text=True)

    def ask(self, **order):
        self.process.stdin.write(json.dumps(order) + '\n')
        self.process.stdin.flush()
        line = self.process.stdout.readline()
        if not line:
            raise Failed('the rehearsal\'s sandbox supervisor stopped')
        return json.loads(line)

    def spawn(self, argv, env, log):
        return self.ask(op='spawn', argv=list(map(str, argv)), env=env, log=str(log))['pid']

    def exited(self, pid, within):
        """The exit code of `pid`, or None while it still runs after `within` seconds."""
        return self.ask(op='wait', pid=pid, timeout=within)['code']

    def run(self, argv, env, timeout):
        return self.ask(op='run', argv=list(map(str, argv)), env=env, timeout=timeout)

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            self.process.wait()


def supervise():
    children = {}
    for line in sys.stdin:
        order = json.loads(line)
        if order['op'] == 'spawn':
            with open(order['log'], 'ab') as output:
                child = subprocess.Popen(order['argv'], env=order['env'], stdin=subprocess.DEVNULL, stdout=output, stderr=subprocess.STDOUT)
            children[child.pid] = child
            answer = {'pid': child.pid}
        elif order['op'] == 'wait':
            try:
                answer = {'code': children[order['pid']].wait(order['timeout'])}
            except subprocess.TimeoutExpired:
                answer = {'code': None}
        else:
            try:
                done = subprocess.run(order['argv'], env=order['env'], stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                      timeout=order['timeout'])
                answer = {'code': done.returncode, 'stdout': done.stdout, 'stderr': done.stderr}
            except subprocess.TimeoutExpired:
                answer = {'code': None, 'stdout': '', 'stderr': f'did not finish within {order["timeout"]} s'}
        print(json.dumps(answer), flush=True)


class CaseDaemon:
    """A daemon the supervisor started, on the case's fixed port."""

    def __init__(self, supervisor, launch_path, env, port, log):
        self.supervisor, self.port, self.log = supervisor, port, log
        self.pid = supervisor.spawn([launch_path], env, log)

    def status(self):
        try:
            with urllib.request.urlopen(f'http://127.0.0.1:{self.port}/daemon', timeout=0.5) as response:
                return json.loads(response.read())
        except (OSError, ValueError):
            return None

    def wait_for_version(self, ver, within):
        deadline = time.monotonic() + within
        while time.monotonic() < deadline:
            code = self.supervisor.exited(self.pid, 0)
            if code is not None:
                raise Failed(f'the daemon exited {code}: {last_line(self.log.read_text())}')
            if (self.status() or {}).get('version') == ver:
                return
            time.sleep(0.05)
        raise Failed(f'the daemon did not answer as {ver} within {within} s: {last_line(self.log.read_text())}')

    def kill(self):
        os.kill(self.pid, signal.SIGKILL)
        self.supervisor.exited(self.pid, 30)

    def stop(self):
        if self.supervisor.exited(self.pid, 0) is None:
            os.kill(self.pid, signal.SIGTERM)
            if self.supervisor.exited(self.pid, 10) is None:
                self.kill()


class HandoverWorld:
    """The previous release, installed as Homebrew does, with Claude Code and GitHub replaced by
    the sandbox's fakes, a phone paired over the direct listener, and the SDK backend the new
    release's installation would write."""

    def __init__(self, rehearsal, root, sdk):
        self.rehearsal, self.root, self.sdk = rehearsal, root, sdk
        self.home = root / 'home'
        self.fakes = root / 'fakes'
        self.claude_home = root / 'fakehome/.claude'
        self.state_root = root / 'h'
        self.runtime = root / 'sdk'
        self.capture = self.runtime / 'capture'
        self.host_gate = self.runtime / 'hold-host'
        self.port, self.direct_port = free_port(), free_port()
        self.log = root / 'alondrad.log'
        self.daemon = None
        self.supervisor = Supervisor(rehearsal)

    def env(self):
        env = self.rehearsal.env(self.home, self.port)
        env.update(HOME=str(self.root / 'fakehome'), ALONDRA_WAKE='1', ALONDRA_IGNORE_MAC_LOAD='1',
                   PATH=f'{self.fakes}:{self.root / "stubs"}:/usr/bin:/bin:/usr/sbin:/sbin')
        return env

    def tool(self, *args, timeout=120):
        result = self.supervisor.run([self.root / 'bin/alondra-sandbox', '--sandbox', self.root, *args], self.env(), timeout)
        if result['code'] != 0:
            raise Failed(f'alondra-sandbox {" ".join(map(str, args[:3]))}: {last_line(result["stdout"] + result["stderr"])}')
        return result['stdout']

    def lay_out(self, old, new_keg, make_new_keg):
        """The Mac with `old` installed and `new_keg` ready to take its place, or with only the new
        release installed when there is no `old`."""
        root = self.root
        for directory in ('bin', 'fakes/scripts', 'stubs', 'fakehome/.claude/plugins', 'opt'):
            (root / directory).mkdir(parents=True)
        self.state_root.mkdir(mode=0o700)
        make_new_keg(new_keg)
        if old:
            old_keg = root / 'Cellar/alondra' / old['version']
            keg_from_archive(old['archive'], old_keg, published=True)
            link(root / 'opt/alondra', old_keg)
        else:
            link(root / 'opt/alondra', new_keg)
        shutil.copy2(self.sdk.tool, root / 'bin/alondra-sandbox')
        (root / 'bin/alondra').symlink_to(root / 'opt/alondra/bin/alondra')
        for name, command in (('claude', 'fake-claude'), ('gh', 'fake-gh')):
            write_executable(self.fakes / name, f'#!/bin/sh\nCLAUDE_HOME="{self.claude_home}" exec "{root}/bin/alondra-sandbox" '
                                                 f'--sandbox "{root}" {command} -- "$@"\n')
        # The plugin's wrapper finds the CLI through Homebrew's prefix, as on a Mac that installed it.
        write_executable(self.fakes / 'brew', f'#!/bin/sh\nprintf "%s\\n" "brew $*" >> "{root}/stub-calls.log"\n'
                                              f'[ "$1" = --prefix ] && echo "{root}"\nexit 0\n')
        for name in STUBBED_FOR_FAKES:
            write_executable(root / 'stubs' / name, f'#!/bin/sh\nprintf "%s\\n" "{name} $*" >> "{root}/stub-calls.log"\nexit 0\n')
        # Signed in as the sandbox's fake answers `claude auth status`: a signed-out account starts nothing.
        (self.claude_home / '.sandbox-account').write_text('default@sandbox.test\n')
        (self.claude_home / 'plugins/known_marketplaces.json').write_text(
            json.dumps({'alondra': {'installLocation': str(root / 'opt/alondra/libexec/alondra')}}))
        repo = root / 'repo'
        git = ['git', '-c', 'user.name=Alondra Rehearsal', '-c', 'user.email=rehearsal@alondra.invalid', '-c', 'commit.gpgsign=false']
        subprocess.run([*git, 'init', '-q', '--bare', '-b', 'main', root / 'remote.git'], check=True)
        subprocess.run([*git, 'init', '-q', '-b', 'main', repo], check=True)
        (repo / 'README.md').write_text('# Rehearsal repository\n')
        for args in (('add', 'README.md'), ('commit', '-q', '-m', 'Start the rehearsal repository'),
                     ('remote', 'add', 'origin', root / 'remote.git'), ('push', '-q', '-u', 'origin', 'main')):
            subprocess.run([*git, '-C', repo, *args], check=True)
        (root / 'fakehome/.claude.json').write_text(json.dumps({'projects': {str(repo.resolve()): {'hasTrustDialogAccepted': True}}}))
        (root / 'sandbox.json').write_text(json.dumps({'relay': 'http://127.0.0.1:9/', 'issuer': 'http://127.0.0.1:9', 'fakes': True,
            'macs': [{'number': 1, 'home': str(self.home), 'port': self.port, 'direct_port': self.direct_port, 'iroh_port': 0}]}))

    def provision(self):
        """The Mac's identity and direct listener, so the phone pairs with it without a relay; the
        CLI finds this daemon's port in config.toml, never the real one."""
        self.tool('provision-mac', self.home, '--relay', 'http://127.0.0.1:9/', '--direct-port', self.direct_port)
        (self.home / 'config.toml').write_text(f'port = {self.port}\n')

    def start(self, version):
        self.daemon = CaseDaemon(self.supervisor, self.root / 'opt/alondra/bin/alondrad', self.env(), self.port, self.log)
        self.daemon.wait_for_version(version, SERVE_WITHIN)

    def crash(self):
        """SIGKILL: the daemon dies at once, its nodes and SDK hosts go on."""
        self.daemon.kill()

    def connect_claude(self):
        """What `alondra doctor --fix` does on a new install when the user picks Claude: a new HOME
        has no provider connected, and no request runs until one is."""
        listing = self.settings('GET', '/settings/accounts')
        config = listing['config']
        connected = config.get('connected') or []
        config['connected'] = connected if 'claude' in connected else [*connected, 'claude']
        self.settings('PUT', '/settings/accounts', {'version': listing['version'], 'accounts': config})
        self.settings('POST', '/settings/accounts/check')

    def settings(self, method, path, body=None):
        request = urllib.request.Request(f'http://127.0.0.1:{self.port}{path}?capabilities=provider_accounts', method=method,
                                         data=None if body is None else json.dumps(body).encode(),
                                         headers={'Content-Type': 'application/json'})
        with urllib.request.urlopen(request, timeout=PATIENCE) as response:
            return json.loads(response.read())

    def request(self, task, title):
        script = {'request_id': f'req-{task}', 'tasks': [{'task_id': task, 'title': title}]}
        (self.fakes / 'scripts' / f'req-{task}.json').write_text(json.dumps(script))
        self.tool('phone', '--mac', '1', 'request', '--direct', 'POST', '/requests', json.dumps({'request_id': f'req-{task}', 'text': title}))

    def press(self, view, action, text=None):
        self.tool('phone', '--mac', '1', 'press', '--direct', view, action, *(('--text', text) if text else ()))

    def node(self, task):
        found = [read_json(path) for path in sorted((self.fakes / 'nodes').glob('*/node.json'))]
        found = [node for node in found if node.get('task_id') == task]
        return found[-1] if found else None

    def bound(self, task):
        node = self.node(task)
        if not node or node['state'] != 'running':
            return False
        settings = read_json(pathlib.Path(node['settings']))
        return 'ALONDRA_DOMAIN_CAPABILITY' in settings.get('env', {})

    def settled(self, task):
        """The node finished starting its first turn: the fake reads around ten times before it
        only listens, and a daemon that goes away before that ends its process."""
        hooks = read_lines(self.fakes / 'nodes' / self.node(task)['short'] / 'hooks.jsonl')
        return sum(hook['hook'] == 'PostToolUse' for hook in hooks) >= 10

    def act(self, task, verb):
        """The step the node took, as `fake node` prints it: `ok <step as JSON>: <what Alondra said>`."""
        line = last_line(self.tool('fake', 'node', task, verb))
        if not line.startswith('ok '):
            raise Failed(f'the node of {task} could not {verb}: {line}')
        return json.JSONDecoder().raw_decode(line[3:])[0]

    def calls(self):
        return read_lines(self.fakes / 'calls.jsonl')

    def pushed(self, task):
        return [push.get('text') or '' for push in read_lines(self.fakes / 'nodes' / self.node(task)['short'] / 'pushed.jsonl')]

    def task_state(self, task):
        return next((row['state'] for row in rows(self.home, 'tasks') if row['id'] == task), None)

    def verdicts(self):
        with urllib.request.urlopen(f'http://127.0.0.1:{self.port}/sdk-handover', timeout=5) as response:
            report = json.loads(response.read())
        return {row['task']: (row['verdict'], row['reason']['code']) for row in report['tasks']}

    def starts(self, session):
        return [start for start in read_lines(self.runtime / 'capture.starts') if start['session'] == session]

    def transcript(self, session):
        path = self.runtime / f'capture.transcript-{session}'
        return path.read_text().splitlines()[1:] if path.exists() else []

    def install_sdk_backend(self, keg):
        """The host the new release installed in `keg`, which checks its own bytes when it starts,
        with a gate it loads through NODE_OPTIONS that holds its start for the crash after
        `sdk_admitted`, and a native Claude that records every start and the session it resumed:
        the configuration replaces only the user's Claude Code and adds the gate."""
        self.runtime.mkdir()
        host = keg / 'libexec/alondra' / release.HOST
        for name in FIXTURES:
            shutil.copy2(self.sdk.fixtures / name, self.runtime / name)
        write_executable(self.runtime / 'claude', NATIVE)
        (self.runtime / 'gate.cjs').write_text(HOST_GATE)
        environment = {'PATH': '/usr/bin:/bin'}
        host_environment = dict(environment, NODE_OPTIONS=f'--require={self.runtime / "gate.cjs"}',
                                ALONDRA_REHEARSAL_HOST_GATE=str(self.host_gate))
        backend = {
            'node': str(host), 'node_sha256': checksum(host),
            'host': str(host), 'host_sha256': checksum(host),
            'native': str(self.runtime / 'claude'), 'native_sha256': checksum(self.runtime / 'claude'),
            'host_environment': host_environment, 'native_environment': dict(environment, FIXTURE_CAPTURE=str(self.capture)),
            'state_root': str(self.state_root), 'io_timeout_seconds': 300,
        }
        (self.home / 'sdk-backend.json').write_text(json.dumps(backend, indent=2) + '\n')

    def stop_everything(self):
        """Every process the case started: daemons, fake nodes, their connections and SDK hosts."""
        if self.daemon:
            self.daemon.stop()
        self.supervisor.close()
        mine = str(self.root)
        listing = subprocess.run(['/bin/ps', '-axo', 'pid=,command='], capture_output=True, text=True).stdout
        for line in listing.splitlines():
            pid, _, command = line.strip().partition(' ')
            if mine in command and int(pid) != os.getpid():
                try:
                    os.kill(int(pid), signal.SIGKILL)
                except ProcessLookupError:
                    pass


def read_json(path):
    return json.loads(path.read_text())


def write_executable(path, text):
    path.write_text(text)
    path.chmod(0o755)


def sdk_handover(rehearsal, old, new, sdk):
    """The previous release leaves tasks in progress on claude --bg; after the update each node
    moves to the Agent SDK with its same session, by facts, and a SIGKILL after each step of a move
    changes nothing."""
    world = HandoverWorld(rehearsal, rehearsal.work / SDK_CASE, sdk)
    world.root.mkdir()
    try:
        return run_handover(world, old, new)
    except Failed as error:
        raise Failed(f'{error}\n{log_tail(world.log)}') from None
    finally:
        world.stop_everything()


def log_tail(log, lines=40):
    """The end of a case's daemon log, which goes with the rehearsal's folder: each line once, in
    the order it first appeared, so a line repeated every second does not hide the rest."""
    if not log.is_file():
        return f'(no daemon log at {log})'
    seen, kept = set(), []
    for line in log.read_text(errors='replace').splitlines():
        text = line.split(' ', 1)[-1]
        if text not in seen:
            seen.add(text)
            kept.append(line)
    return 'the daemon log ends:\n' + '\n'.join(kept[-lines:])


FRESH_SDK_CASE = 'fresh-sdk'
FRESH_TASK = 't-fresh'


def fresh_sdk(rehearsal, new, sdk):
    """A Mac that installs the new release: no `sdk-backend.json` and no Node on the PATH. A request
    becomes a task whose node runs through the host the release installed, with the Claude Code on
    the daemon's PATH, here the sandbox's fake."""
    world = HandoverWorld(rehearsal, rehearsal.work / FRESH_SDK_CASE, sdk)
    world.root.mkdir()
    try:
        world.lay_out(None, new['keg'], new['make_keg'])
        if shutil.which('node', path=world.env()['PATH']):
            raise Failed('node is on the PATH the daemon gets: the case needs a Mac without it')
        world.home.mkdir(mode=0o700)
        world.provision()
        world.start(new['version'])
        world.connect_claude()
        world.tool('phone', '--mac', '1', 'pair', '--direct')
        world.request(FRESH_TASK, 'Write the changelog')
        session = until('the task runs through the host the release installed', lambda: (fresh_session(world), fresh_sessions(world)), within=PATIENCE)
        agent = next(row['agent_id'] for row in rows(world.home, 'tasks') if row['id'] == FRESH_TASK)
        if not str(agent).startswith('sdk-'):
            raise Failed(f'{FRESH_TASK} runs on {agent}, not on a node of the Agent SDK')
        if (world.home / 'sdk-backend.json').exists():
            raise Failed('the case wrote an sdk-backend.json: the release must run without one')
        old_path = [call for call in world.calls() if call.get('tool') == 'claude' and any(arg in ('--bg', '-p') for arg in call.get('args', []))]
        if old_path:
            raise Failed(f'the new release ran claude the old way: {old_path[0]["args"]}')
        return {'task': FRESH_TASK, 'node': agent, 'session': session['session_id'], 'cwd': session['cwd'], 'host': str(new['keg'] / 'libexec/alondra' / release.HOST)}
    except Failed as error:
        raise Failed(f'{error}\n{log_tail(world.log)}') from None
    finally:
        world.stop_everything()


def fresh_sessions(world):
    return [read_json(path) for path in sorted((world.fakes / 'sessions').glob('*/session.json'))]


def fresh_session(world):
    return next((session for session in fresh_sessions(world) if session.get('task_id') == FRESH_TASK and session.get('starts', 0) >= 1), None)


def restart_into(world, old, new):
    """Homebrew's upgrade: opt points at the new keg, and the previous daemon restarts into it by
    itself. Returns when, in Unix milliseconds, opt changed."""
    link(world.root / 'opt/alondra', new['keg'])
    swapped = time.time() * 1000
    code = world.supervisor.exited(world.daemon.pid, EXIT_WITHIN)
    if code is None:
        raise Failed(f'{old["version"]} did not restart into {new["version"]} within {EXIT_WITHIN} s')
    if code != RELAUNCH:
        raise Failed(f'{old["version"]} exited {code} instead of {RELAUNCH}: {last_line(world.log.read_text())}')
    return swapped


def run_handover(world, old, new):
    world.lay_out(old, new['keg'], new['make_keg'])
    fresh_previous(world.rehearsal, world.home, old)
    world.provision()
    world.start(old['version'])
    world.tool('phone', '--mac', '1', 'pair', '--direct')
    for task, title in SEEDED.items():
        world.request(task, title)
    for task in SEEDED:
        until(f'{old["version"]} launches the claude --bg node of {task}', lambda task=task: (world.bound(task), world.node(task)), within=PATIENCE * 2)
    until('every node is past the start of its first turn', lambda: (all(world.settled(task) for task in SEEDED),
                                                                     {task: world.settled(task) for task in SEEDED}))
    cards = {task: world.act(task, 'ask')['id'] for task in (RESTING, CRASHING)}
    until('the asking nodes wait for the user', lambda: (all(world.task_state(task) == 'needs_you' for task in cards),
                                                       {task: world.task_state(task) for task in SEEDED}))
    sessions = {task: world.node(task)['session_id'] for task in SEEDED}
    world.install_sdk_backend(new['keg'])
    for session in sessions.values():
        (world.runtime / f'capture.transcript-{session}').write_text(TRANSCRIPT)
    before = domain_snapshot(world.home)
    swapped = restart_into(world, old, new)
    world.act(LOST, 'die')
    world.start(new['version'])
    changes = snapshot_changes(before, domain_snapshot(world.home))
    if changes:
        raise Failed(f'the update changed tasks, requests or cards: {"; ".join(changes[:5])}')
    expected = {RESTING: ('passes', 'next_message'), CRASHING: ('passes', 'next_message'), WORKING: ('waits', 'in_turn')}
    until('the new daemon judges each node by its facts', lambda: (all(world.verdicts().get(task) == verdict for task, verdict in expected.items()),
                                                                   world.verdicts()))
    if journal(world.home):
        raise Failed(f'a node moved before its next input: {journal(world.home)}')
    for task in (RESTING, CRASHING, WORKING):
        if not alive(world.node(task)['pid']):
            raise Failed(f'the claude --bg node of {task} stopped with the update')
    until(f'the lost node of {LOST} moves to the Agent SDK as Alondra brings it back',
          lambda: (journal(world.home).get(sessions[LOST]) == list(HANDOVER_STEPS), journal(world.home)))
    crashes = crash_through_each_step(world, sessions[CRASHING], cards[CRASHING], new['version'])
    world.press(f'card:{cards[RESTING]}', ANSWER)
    until(f'the node of {RESTING} moves with the answer to its card',
          lambda: (journal(world.home).get(sessions[RESTING]) == list(HANDOVER_STEPS), journal(world.home)))
    world.press(f'task:{WORKING}', 'msg:', DURING_TURN)
    until(f'the node of {WORKING} takes the message inside its turn on claude --bg',
          lambda: (any(DURING_TURN in text for text in world.pushed(WORKING)), world.pushed(WORKING)))
    until(f'the node of {WORKING} rests once its turn ended',
          lambda: (world.verdicts().get(WORKING, ('',))[0] == 'passes', world.verdicts().get(WORKING)))
    if sessions[WORKING] in journal(world.home):
        raise Failed(f'the node of {WORKING} moved while it was in a turn')
    world.press(f'task:{WORKING}', 'msg:', AFTER_TURN)
    until(f'the node of {WORKING} moves with its next input',
          lambda: (journal(world.home).get(sessions[WORKING]) == list(HANDOVER_STEPS), journal(world.home)))
    batch = run_orchestrator_batch(world)
    inputs = {RESTING: ANSWER, CRASHING: ANSWER, WORKING: AFTER_TURN, LOST: BROUGHT_BACK}
    for task, text in inputs.items():
        check_moved(world, task, sessions[task], text)
    late = [call for call in world.calls() if call['at'] >= swapped and call['tool'] == 'claude'
            and ('--bg' in call['args'] or 'respawn' in call['args'] or '-p' in call['args'])]
    if late:
        raise Failed(f'after the update Alondra still ran claude {" ".join(late[0]["args"][:3])}')
    if not (world.home / 'domain.db.before-sdk-handover').is_file():
        raise Failed('domain.db was not kept as domain.db.before-sdk-handover before the first move')
    return {'moved': sorted(inputs), 'crashed_after': crashes, 'orchestrator_batch': batch}


def crash_through_each_step(world, session, card, version):
    """The answer to its card moves the node. Before each step a fact the next one needs is held
    back, so the move stops right after recording the step: `claude stop` fails, the SDK state
    root takes no new host folder, the SDK host waits before it starts while the daemon waits for
    it. The daemon dies there by SIGKILL and comes back with that fact restored and the next held."""
    stop_held = world.fakes / 'hold-stop'
    holds = [
        ('planned', stop_held.touch, stop_held.unlink, lambda: True),
        ('bg_stopped', lambda: world.state_root.chmod(0o500), lambda: world.state_root.chmod(0o700), lambda: True),
        ('sdk_admitted', world.host_gate.touch, world.host_gate.unlink, pathlib.Path(f'{world.host_gate}.held').exists),
    ]
    crashed = []
    holds[0][1]()
    world.press(f'card:{card}', ANSWER)
    for at, (step, _, restore, stopped_there) in enumerate(holds):
        until(f'the move of {CRASHING} stops at {step}',
              lambda step=step, stopped_there=stopped_there: ((journal(world.home).get(session) or [None])[-1] == step and stopped_there(),
                                                              journal(world.home)))
        world.crash()
        crashed.append(step)
        restore()
        if at + 1 < len(holds):
            holds[at + 1][1]()
        world.start(version)
    until(f'the move of {CRASHING} ends after its crashes', lambda: (journal(world.home).get(session) == list(HANDOVER_STEPS), journal(world.home)))
    return crashed


def run_orchestrator_batch(world):
    """A request after the update: its batch runs through the SDK host, a new conversation."""
    world.tool('phone', '--mac', '1', 'request', '--direct', 'POST', '/requests',
               json.dumps({'request_id': LATER_REQUEST, 'text': 'Plan the next release'}))
    prompts = pathlib.Path(f'{world.capture}.orchestrator.prompts')
    until('the next orchestrator batch runs through the Agent SDK',
          lambda: (prompts.exists() and LATER_REQUEST in prompts.read_text(), prompts.exists()))
    arguments = read_json(pathlib.Path(f'{world.capture}.orchestrator'))['args']
    session = [argument.split('=', 1)[1] for argument in arguments if argument.startswith('--session-id=')]
    if len(session) != 1 or any(argument.startswith('--resume') for argument in arguments):
        raise Failed(f'the orchestrator batch through the SDK did not start a new conversation: {arguments}')
    return {'session': session[0]}


def check_moved(world, task, session, first_input):
    """Its SDK node resumed the same Claude session once, in the node's folder, with the input
    that moved it as its first and only copy; the claude --bg process is gone and never got it."""
    node = world.node(task)
    starts = world.starts(session)
    if len(starts) != 1:
        raise Failed(f'the SDK started the session of {task} {len(starts)} times')
    start = starts[0]
    folder = pathlib.Path(node.get('worktree') or node['cwd']).resolve()
    if start['resume'] != session or start['history'] != 1 or pathlib.Path(start['cwd']).resolve() != folder:
        raise Failed(f'the SDK did not resume the session of {task} in its folder with its transcript: {start}')
    copies = sum(first_input in line for line in world.transcript(session))
    if copies != 1:
        raise Failed(f'the input that moved {task} reached its session {copies} times')
    if any(first_input in text for text in world.pushed(task)):
        raise Failed(f'the input that moved {task} also reached its claude --bg node')
    if node['state'] not in ('stopped', 'dead') or alive(node['pid']):
        raise Failed(f'the claude --bg node of {task} still runs after its move')


UPGRADE_CASE = 'sdk-upgrade'
# The tasks the previous release leaves with a command running in the background: the node that
# moves to a host of the new release once it rests, and the one that moves too while the new daemon
# dies before and after its move.
KEEPS, CRASHES = 't-keeps', 't-crashes'
UPGRADE_SEEDED = {KEEPS: 'Run the slow check', CRASHES: 'Run the long build'}
BACKGROUND_COMMAND = 'background-command'
AFTER_MOVE = 'Say what the command found'
UPGRADE_CRASHES = ('adopted', 'moved')
# The cards of a task whose host could not be taken over, whose session ended mid-way, or whose
# input its host may have lost.
MOVE_CARDS = ('host-lost-', 'session-ended-', 'handoff-doubt-')
TURN_ENDED = 'turn-ended-'
# Claude Code on the daemon's PATH when the release runs its own host and finds `claude` there.
ROUTER = '''#!/usr/bin/python3
"""A task's session, as the Agent SDK host spawns it, runs the persistent fixture with a capture of
its own; the version and sign-in checks, an orchestrator batch and the usage probe run the
sandbox's claude."""
import os, sys
# paths
SPAWN = ['--output-format', 'stream-json', '--verbose', '--input-format', 'stream-json']
args = sys.argv[1:]
def flag(name):
    for at, argument in enumerate(args):
        if argument.startswith(name + '='):
            return argument.split('=', 1)[1]
        if argument == name and at + 1 < len(args):
            return args[at + 1]
    return None
session = flag('--resume') or flag('--session-id')
if args[:len(SPAWN)] == SPAWN and session and not {'--restricted', '--strict-mcp-config'} & set(args):
    os.environ['FIXTURE_CAPTURE'] = os.path.join(CAPTURES, 'capture-' + session)
    os.execv('/usr/bin/python3', ['/usr/bin/python3', FIXTURE] + args)
os.execv(SANDBOX, [SANDBOX] + args)
'''


def router(captures, fixture, sandbox):
    return ROUTER.replace('# paths\n', f'CAPTURES, FIXTURE, SANDBOX = {str(captures)!r}, {str(fixture)!r}, {str(sandbox)!r}\n')


def identity(pid):
    """A process as `ps` sees it, its PID with its start time, so a PID given to another process
    later is not taken for it; None once it is gone."""
    if pid is None:
        return None
    shown = subprocess.run(['/bin/ps', '-o', 'stat=,lstart=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
    if not shown or shown.startswith('Z'):
        return None
    return (pid, shown.split(None, 1)[1])


def parent(pid):
    shown = subprocess.run(['/bin/ps', '-o', 'ppid=', '-p', str(pid)], capture_output=True, text=True).stdout.strip()
    return int(shown) if shown else None


def lock_holders(lock, marker=pathlib.PurePath(release.HOST).name):
    """The processes named `marker` that hold `lock` open: an SDK host keeps its host.lock as its
    stdin, and its flock, for as long as it runs."""
    listed = subprocess.run(['/usr/sbin/lsof', '-t', '--', str(lock)], capture_output=True, text=True).stdout.split()
    holders = []
    for pid in sorted(set(map(int, listed))):
        command = subprocess.run(['/bin/ps', '-o', 'command=', '-p', str(pid)], capture_output=True, text=True).stdout
        found = identity(pid) if marker in command else None
        if found:
            holders.append(found)
    return holders


def bootstrap(folder):
    """What the daemon gave the host of `folder` (its session, its release), which the host.lock
    holds; None while it is being written."""
    try:
        return json.loads((folder / 'host.lock').read_text())
    except (OSError, ValueError):
        return None


def session_hosts(state_root, session):
    """The folder of the host that runs `session` under the SDK state root, and the folders of its
    hosts that ended and were retired beside it as `<folder>.ended-<host>`."""
    live, ended = None, []
    for folder in sorted(state_root.iterdir()) if state_root.is_dir() else []:
        config = bootstrap(folder) if folder.is_dir() else None
        if not config or (config.get('native') or {}).get('session_id') != session:
            continue
        if '.ended-' in folder.name:
            ended.append(folder)
        else:
            live = folder
    return live, ended


def host_facts(world, session, command):
    """The background command, the host that holds the session's folder, its release, the retired
    folders and every start of the native Claude, as the processes and files show them."""
    live, ended = session_hosts(world.home / 'sdk', session)
    holders = lock_holders(live / 'host.lock') if live else []
    config = (bootstrap(live) if live else None) or {}
    return {'command': identity(command), 'host': holders[0] if holders else None,
            'release': (config.get('manifest') or {}).get('host_release'), 'ended': len(ended),
            'starts': read_lines(world.runtime / f'capture-{session}.starts')}


def kept(task, before, now):
    """What an update or a crash of the new daemon owes a node whose command runs in the
    background: the same command and the same host go on, and Claude does not start again."""
    problems = []
    if now['command'] != before['command']:
        problems.append(f'the command {task} left running in the background stopped: it was {before["command"]}, now {now["command"]}')
    if now['host'] != before['host']:
        problems.append(f'the SDK host of {task} did not go on: it was {before["host"]}, now {now["host"]}')
    if now['ended']:
        problems.append(f'the host folder of {task} was retired {now["ended"]} times')
    if len(now['starts']) != len(before['starts']):
        problems.append(f'the native Claude of {task} started {len(now["starts"]) - len(before["starts"])} more times')
    return problems


def moved(before, now, old_host_runs):
    """The resting node's old host is gone, its folder retired once, and a host of another release
    holds the folder for the same session."""
    return (not old_host_runs and now['host'] is not None and now['host'] != before['host']
            and now['release'] not in (None, before['release']) and now['ended'] == 1)


def turns_ended(interactions, task):
    """How many turns of the task's node ended: each leaves a card of its own."""
    return sum(row['id'].startswith(f'{TURN_ENDED}{task}-') for row in interactions)


def move_cards(interactions, task):
    """The cards that ask the user about a host the update or a move lost."""
    return sorted(row['id'] for row in interactions if row.get('task_id') == task and row['id'].startswith(MOVE_CARDS))


def release_command(capture):
    """Lets the fixture's background command end: it reads one line from the FIFO beside the capture."""
    try:
        fifo = os.open(f'{capture}.release', os.O_WRONLY | os.O_NONBLOCK)
    except OSError as error:
        raise Failed(f'no background command waits on {capture}.release: {error}') from None
    try:
        os.write(fifo, b'done\n')
    finally:
        os.close(fifo)


def new_sessions(runtime, known):
    return sorted({path.name[len('capture-'):-len('.starts')] for path in runtime.glob('capture-*.starts')} - set(known))


def install_router(world):
    """The release's own host with the persistent fixture as Claude Code: `claude` on the daemon's
    PATH sends a task's session to it and everything else to the sandbox's claude."""
    world.runtime.mkdir()
    fixture = world.runtime / FIXTURES[0]
    shutil.copy2(world.sdk.fixtures / FIXTURES[0], fixture)
    sandbox = world.fakes / 'sandbox-claude'
    os.replace(world.fakes / 'claude', sandbox)
    write_executable(world.fakes / 'claude', router(world.runtime, fixture, sandbox))


def sdk_upgrade(rehearsal, old, new, sdk):
    """The previous release leaves nodes on its own SDK hosts with a command running in the
    background; the update keeps each command and host, and each node moves to a host of the new
    release once it rests, resuming its session, also with the new daemon killed around the move."""
    world = HandoverWorld(rehearsal, rehearsal.work / UPGRADE_CASE, sdk)
    world.root.mkdir()
    try:
        return run_upgrade(world, old, new)
    except Failed as error:
        raise Failed(f'{error}\n{log_tail(world.log)}') from None
    finally:
        world.stop_everything()


def background_node(world, task, title, version, known):
    """A task whose node's turn ended behind a command it left running: its session and the
    command's PID."""
    world.request(task, title)

    def started():
        found = new_sessions(world.runtime, known)
        return (found[0] if len(found) == 1 else None, found)
    session = until(f'{version} starts the node of {task} on its own Agent SDK host', started, within=PATIENCE * 2)
    until(f'the first turn of {task} ends', lambda: (turns_ended(rows(world.home, 'interactions'), task) >= 1, None))
    world.press(f'task:{task}', 'msg:', BACKGROUND_COMMAND)
    written = world.runtime / f'capture-{session}.background'

    def left_running():
        text = written.read_text().strip() if written.is_file() else ''
        return (int(text) if text.isdigit() else None, {'pid': text})
    return session, until(f'the node of {task} leaves a command running in the background', left_running)


def check_kept(world, sessions, commands, before, after_what):
    problems = [problem for task in sessions for problem in kept(task, before[task], host_facts(world, sessions[task], commands[task]))]
    if problems:
        raise Failed(f'after {after_what}: {"; ".join(problems)}')


def move_at_rest(world, task, session, command, before):
    """The command ends, the node rests, and the new daemon puts a host of its release in the old
    one's place. The new host, as `identity` shows it."""
    release_command(world.runtime / f'capture-{session}')

    def probe():
        now = host_facts(world, session, command)
        old_host_runs = identity(before['host'][0]) == before['host']
        return (now['host'] if moved(before, now, old_host_runs) else None,
                {'old host runs': old_host_runs, **{key: now[key] for key in ('command', 'host', 'release', 'ended')}})
    return until(f'once the node of {task} rests, a host of the new release takes the place of its old one', probe)


def resume_after_move(world, task, session, host):
    """A message reaches the node: Claude starts again once, resuming the same session under the
    host the move launched."""
    world.press(f'task:{task}', 'msg:', AFTER_MOVE)
    recorded = world.runtime / f'capture-{session}.starts'
    until(f'the message to {task} resumes its Claude session', lambda: (len(read_lines(recorded)) > 1, read_lines(recorded)))
    starts = read_lines(recorded)
    start = starts[1]
    if len(starts) != 2 or start['resume'] != session or not start['history']:
        raise Failed(f'the native Claude of {task} did not resume its session once: {starts}')
    if parent(start['pid']) != host[0]:
        raise Failed(f'the resumed Claude of {task} does not run under the host the move launched ({host[0]})')
    now = host_facts(world, session, None)
    if now['host'] != host or now['ended'] != 1:
        raise Failed(f'the host of {task} changed again after its move: {now["host"]}, {now["ended"]} retired folders')


def run_upgrade(world, old, new):
    world.lay_out(old, new['keg'], new['make_keg'])
    install_router(world)
    fresh_previous(world.rehearsal, world.home, old)
    world.provision()
    world.start(old['version'])
    world.tool('phone', '--mac', '1', 'pair', '--direct')
    sessions, commands = {}, {}
    for task, title in UPGRADE_SEEDED.items():
        sessions[task], commands[task] = background_node(world, task, title, old['version'], sessions.values())
    before = {task: host_facts(world, sessions[task], commands[task]) for task in UPGRADE_SEEDED}
    for task, facts in before.items():
        if None in (facts['command'], facts['host']) or facts['ended'] or len(facts['starts']) != 1:
            raise Failed(f'{old["version"]} did not leave {task} with one host and its command running: {facts}')
    restart_into(world, old, new)
    shutil.rmtree(world.root / 'Cellar/alondra' / old['version'])
    world.start(new['version'])
    check_kept(world, sessions, commands, before, f'the update to {new["version"]}')
    world.crash()
    world.start(new['version'])
    check_kept(world, sessions, commands, before, 'the new daemon was killed and came back')
    crashed = ['adopted']
    hosts = {KEEPS: move_at_rest(world, KEEPS, sessions[KEEPS], commands[KEEPS], before[KEEPS])}
    resume_after_move(world, KEEPS, sessions[KEEPS], hosts[KEEPS])
    hosts[CRASHES] = move_at_rest(world, CRASHES, sessions[CRASHES], commands[CRASHES], before[CRASHES])
    world.crash()
    world.start(new['version'])
    crashed.append('moved')
    resume_after_move(world, CRASHES, sessions[CRASHES], hosts[CRASHES])
    asked = {task: move_cards(rows(world.home, 'interactions'), task) for task in UPGRADE_SEEDED}
    if any(asked.values()):
        raise Failed(f'the update or a move asked the user about a host: {asked}')
    return {'tasks': {task: {'session': sessions[task], 'command': commands[task]} for task in UPGRADE_SEEDED},
            'host_release': {'before': before[KEEPS]['release'], 'after': host_facts(world, sessions[KEEPS], None)['release']},
            'crashed_after': crashed}


def sdk_cases(previous):
    """A release before the Agent SDK leaves claude --bg nodes to move; one after it, SDK hosts to
    keep. Every release also installs fresh."""
    return (UPGRADE_CASE if has_sdk_backend(previous) else SDK_CASE, FRESH_SDK_CASE)


ALL_CASES = (*CASES, SDK_CASE, UPGRADE_CASE, FRESH_SDK_CASE)


def chosen_cases(previous, sdk, only=None):
    """The cases an upgrade from `previous` runs, in order, or only those of `only`; naming one that
    does not run from `previous` is an error."""
    planned = (*CASES, *(sdk_cases(previous) if sdk else ()))
    if only:
        missing = sorted(set(only) - set(planned))
        if missing:
            raise Failed(f'{", ".join(missing)} does not run when upgrading from {previous}')
    return [case for case in planned if not only or case in only]


def rehearse(new_binary, previous_archive, live_home, work, old_version, new_keg=None, sdk=None, only=None):
    protected = pathlib.Path.home().resolve()
    if not inside(live_home, protected):
        raise Failed(f'The live HOME {live_home} must be inside {protected}, which the sandbox keeps read-only')
    rehearsal = Rehearsal(work, protected, live_home)
    old_binary = unpack(previous_archive, work / 'previous', published=True)
    old = {'version': old_version, 'binary': old_binary, 'archive': previous_archive}
    if binary_version(old_binary) != old_version:
        raise Failed(f'{previous_archive.name} does not contain alondrad {old_version}')
    new = {'version': binary_version(new_binary), 'binary': new_binary}
    if new['version'] == old_version:
        raise Failed(f'The new build reports {old_version}, the version it should replace')
    result = {'schema': 1, 'from': old_version, 'to': new['version'], 'cases': {}}
    chosen = chosen_cases(old_version, sdk, only)
    generic = [case for case in CASES if case in chosen]
    if generic:
        try:
            result['live_preflight_ms'] = rehearsal.preflight(new_binary, live_home)
        except Failed as error:
            raise Failed(f'live HOME {live_home}: {error}') from None
        print(f'live HOME: {new["version"]} preflight passes ({result["live_preflight_ms"]} ms)')
    for case in generic:
        home = work / case / 'home'
        (work / case).mkdir()
        mutate = MUTATIONS.get(case)
        try:
            if case == 'fresh-previous':
                fresh_previous(rehearsal, home, old)
            else:
                rehearsal.copy(live_home, home)
            checked = work / case / 'preflight-home'
            checked.mkdir()
            clone(home.iterdir(), checked)
            if mutate:
                mutate(checked)
            before = digest(checked)
            preflight_ms = rehearsal.preflight(new_binary, checked)
            after = digest(checked)
            if after != before:
                changed = sorted(name for name in before.keys() | after.keys() if before.get(name) != after.get(name))
                raise Failed(f'preflight changed the HOME it checked: {", ".join(changed[:5])}')
            shutil.rmtree(checked)
            timings = {'preflight_ms': preflight_ms, **upgrade(rehearsal, case, home, old, new, mutate)}
        except Failed as error:
            raise Failed(f'case {case}: {error}') from None
        result['cases'][case] = timings
        print(f'{case}: {old_version} → {new["version"]} restarted {timings["exited_after_swap_ms"]} ms after '
              f'the swap and answered in {timings["answered_ms"]} ms')
    if SDK_CASE in chosen:
        keg = work / SDK_CASE / 'Cellar/alondra' / new['version']
        try:
            result['cases'][SDK_CASE] = sdk_handover(rehearsal, old, {**new, 'keg': keg, 'make_keg': new_keg}, sdk)
        except Failed as error:
            raise Failed(f'case {SDK_CASE}: {error}') from None
        moved = result['cases'][SDK_CASE]
        print(f'{SDK_CASE}: {len(moved["moved"])} claude --bg nodes moved to the Agent SDK with their sessions, '
              f'the daemon killed after {", ".join(moved["crashed_after"])}')
    if UPGRADE_CASE in chosen:
        keg = work / UPGRADE_CASE / 'Cellar/alondra' / new['version']
        try:
            result['cases'][UPGRADE_CASE] = sdk_upgrade(rehearsal, old, {**new, 'keg': keg, 'make_keg': new_keg}, sdk)
        except Failed as error:
            raise Failed(f'case {UPGRADE_CASE}: {error}') from None
        kept_running = result['cases'][UPGRADE_CASE]
        print(f'{UPGRADE_CASE}: {len(kept_running["tasks"])} commands in the background outlived the update on the hosts of '
              f'{old_version}; each node moved to a host of {new["version"]} once it rested, resuming its session, '
              f'the daemon killed after {", ".join(kept_running["crashed_after"])}')
    if FRESH_SDK_CASE in chosen:
        keg = work / FRESH_SDK_CASE / 'Cellar/alondra' / new['version']
        try:
            result['cases'][FRESH_SDK_CASE] = fresh_sdk(rehearsal, {**new, 'keg': keg, 'make_keg': new_keg}, sdk)
        except Failed as error:
            raise Failed(f'case {FRESH_SDK_CASE}: {error}') from None
        print(f'{FRESH_SDK_CASE}: a fresh {new["version"]} ran {FRESH_TASK} through its own Agent SDK host, without Node on the PATH')
    return result


def keep_alive_in_plist():
    plist = release.ROOT / 'launchd/alondra.plist.in'
    if not re.search(r'<key>KeepAlive</key>\s*<true/>', plist.read_text()):
        raise Failed(f'{plist} no longer restarts the daemon on every exit (KeepAlive true)')


EPILOG = '''Only one case, for local iteration: what `make upgrade-rehearsal` runs, after its build, with --case:
  files() { bazelisk cquery --output=files "$1" 2>/dev/null | grep -v '\\.ok$'; }
  python3 deploy/brew/upgrade-rehearsal.py --new "$(files //crates/alondrad)" --cli "$(files //crates/alondra)" \\
    --host "$(files //agent-host:release_host | grep 'alondra-host$')" --sdk-fixtures agent-host \\
    --sandbox-tool "$(files //tools/sandbox:alondra-sandbox)" --from latest --case sdk-upgrade
sdk-handover runs from a release before 0.3.0, sdk-upgrade from 0.3.0 on.'''


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0], epilog=EPILOG,
                                     formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--new', type=pathlib.Path, help='Release archive, or an alondrad binary')
    parser.add_argument('--cli', type=pathlib.Path, help='The alondra CLI built with an alondrad binary given as --new')
    parser.add_argument('--host', type=pathlib.Path, help='The release host built with an alondrad binary given as --new (Bazel //agent-host:release_host)')
    parser.add_argument('--sdk-fixtures', type=pathlib.Path, help='The folder with the native Claude fixtures (agent-host)')
    parser.add_argument('--sandbox-tool', type=pathlib.Path, help='alondra-sandbox, whose fakes stand in for Claude Code and GitHub')
    parser.add_argument('--from', dest='previous',
                        help='Published version to upgrade from, or latest (default: the newest one older than --new)')
    parser.add_argument('--previous-archive', type=pathlib.Path, help='Use this archive of --from instead of downloading it')
    parser.add_argument('--home', type=pathlib.Path, default=pathlib.Path.home() / '.alondra')
    parser.add_argument('--output', type=pathlib.Path, help='Where to write the result as JSON')
    parser.add_argument('--case', action='append', choices=ALL_CASES,
                        help='Run only this case (repeatable), for local iteration; writes no result')
    parser.add_argument('--copy-home', nargs=2, type=pathlib.Path, help=argparse.SUPPRESS)
    parser.add_argument('--supervise', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.copy_home:
        copy_home(*args.copy_home)
        return
    if args.supervise:
        supervise()
        return
    if args.new is None:
        parser.error('--new is required')
    if args.case and args.output:
        parser.error('--case runs part of the rehearsal and writes no result: leave out --output')
    keep_alive_in_plist()
    work = pathlib.Path(tempfile.mkdtemp(prefix='alondra-ur-', dir='/private/tmp'))
    try:
        archive = args.new.resolve() if args.new.name.endswith('.tar.gz') else None
        if not archive and None in (args.cli, args.host):
            parser.error('--cli and --host are required with an alondrad binary')
        if None in (args.sdk_fixtures, args.sandbox_tool):
            parser.error(f'--sdk-fixtures and --sandbox-tool are required: the {SDK_CASE} case needs them')
        sdk = SdkArtifacts(args.sdk_fixtures, args.sandbox_tool)
        new_binary = unpack(archive, work / 'new') if archive else args.new.resolve()
        if archive:
            new_keg = lambda keg: keg_from_archive(archive, keg, published=False)
        else:
            new_keg = lambda keg: keg_from_build(new_binary, args.cli.resolve(), args.host.resolve(), keg)
        new_version = binary_version(new_binary)
        old_version = args.previous if args.previous not in (None, 'latest') else \
            previous_version(args.previous or new_version, published_tags())
        previous = args.previous_archive.resolve() if args.previous_archive else fetch_previous(old_version, work)
        result = rehearse(new_binary, previous, args.home.expanduser().resolve(), work, old_version, new_keg, sdk, args.case)
        if archive:
            result['archive'] = {'name': archive.name, 'sha256': release.sha(archive)}
        if args.output:
            args.output.write_text(json.dumps(result, indent=2) + '\n')
        print(f'Upgrade rehearsal passed: {old_version} → {new_version}')
    finally:
        shutil.rmtree(work, ignore_errors=True)


if __name__ == '__main__':
    try:
        main()
    except (Failed, OSError, ValueError, subprocess.CalledProcessError) as error:
        print(f'Upgrade rehearsal failed: {error}', file=sys.stderr)
        sys.exit(1)
