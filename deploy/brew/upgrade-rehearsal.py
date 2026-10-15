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
HEALTH_WITHIN = 1.0
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
    allowed = (release.PUBLIC_FILES, release.PRE_LAUNCHER_FILES) if published else (release.PUBLIC_FILES,)
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
        relaunched.wait_for_version(new['version'], HEALTH_WITHIN)
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


def rehearse(new_binary, previous_archive, live_home, work, old_version):
    protected = pathlib.Path.home().resolve()
    if not inside(live_home, protected):
        raise Failed(f'The live HOME {live_home} must be inside {protected}, which the sandbox keeps read-only')
    rehearsal = Rehearsal(work, protected, live_home)
    old_binary = unpack(previous_archive, work / 'previous', published=True)
    old = {'version': old_version, 'binary': old_binary}
    if binary_version(old_binary) != old_version:
        raise Failed(f'{previous_archive.name} does not contain alondrad {old_version}')
    new = {'version': binary_version(new_binary), 'binary': new_binary}
    if new['version'] == old_version:
        raise Failed(f'The new build reports {old_version}, the version it should replace')
    result = {'schema': 1, 'from': old_version, 'to': new['version'], 'cases': {}}
    try:
        result['live_preflight_ms'] = rehearsal.preflight(new_binary, live_home)
    except Failed as error:
        raise Failed(f'live HOME {live_home}: {error}') from None
    print(f'live HOME: {new["version"]} preflight passes ({result["live_preflight_ms"]} ms)')
    for case in CASES:
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
    return result


def keep_alive_in_plist():
    plist = release.ROOT / 'launchd/alondra.plist.in'
    if not re.search(r'<key>KeepAlive</key>\s*<true/>', plist.read_text()):
        raise Failed(f'{plist} no longer restarts the daemon on every exit (KeepAlive true)')


def main():
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument('--new', type=pathlib.Path, help='Release archive, or an alondrad binary')
    parser.add_argument('--from', dest='previous',
                        help='Published version to upgrade from, or latest (default: the newest one older than --new)')
    parser.add_argument('--previous-archive', type=pathlib.Path, help='Use this archive of --from instead of downloading it')
    parser.add_argument('--home', type=pathlib.Path, default=pathlib.Path.home() / '.alondra')
    parser.add_argument('--output', type=pathlib.Path, help='Where to write the result as JSON')
    parser.add_argument('--copy-home', nargs=2, type=pathlib.Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.copy_home:
        copy_home(*args.copy_home)
        return
    if args.new is None:
        parser.error('--new is required')
    keep_alive_in_plist()
    work = pathlib.Path(tempfile.mkdtemp(prefix='alondra-ur-', dir='/private/tmp'))
    try:
        archive = args.new.resolve() if args.new.name.endswith('.tar.gz') else None
        new_binary = unpack(archive, work / 'new') if archive else args.new.resolve()
        new_version = binary_version(new_binary)
        old_version = args.previous if args.previous not in (None, 'latest') else \
            previous_version(args.previous or new_version, published_tags())
        previous = args.previous_archive.resolve() if args.previous_archive else fetch_previous(old_version, work)
        result = rehearse(new_binary, previous, args.home.expanduser().resolve(), work, old_version)
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
