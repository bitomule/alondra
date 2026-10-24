#!/usr/bin/env python3
"""Owned Tart only: public 018 -> 019 -> restore 018 snapshot -> uninstall.

This deliberately does not claim backward-compatible 019 databases. No login,
launchd setup, Claude or production account is involved.
"""
import json
from contextlib import closing
import errno
import fcntl
import os
from pathlib import Path
import re
import resource
import shutil
import signal
import socket
import sqlite3
import stat
import subprocess
import sys
import time
import urllib.request
import uuid

import cli

def legacy_readiness(process, port, version, home):
    """The pinned 0.1.x daemons answer the /v1 router that later releases retired."""
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        cli.require(process.poll() is None, 'daemon exited before readiness')
        try:
            health = cli.request(port, '/v1/health')
        except (OSError, ValueError):
            time.sleep(.1)
            continue
        cli.require(health.get('ok') is True and health.get('version') == version, 'health/version mismatch')
        config = health['config']
        cli.require(config['home'] == str(home) and config['port'] == port and config['listen'] == '127.0.0.1',
                    'daemon isolation mismatch')
        cli.require(config['wake'] is False and config['legacy_bridge'] is False and config['resume_dead_nodes'] is False,
                    'unsafe daemon flags')
        return health
    raise RuntimeError('daemon readiness timeout')


RELEASES = {
    '0.1.8': {'commit': 'acd39210f921fb305494146e6c717a3f0bc16584',
              'formula': '69fdbe7ddb90c4dec3cba237a2e59ea59efcd7fb',
              'archive': '60f27474a1551db88f0ca273925f728b0235691999ab836e660d7cab60af2d21',
              'alondra': '0906f1864fbc70aa7683c1a54ee011e1760049d83aad1c2511ff3b2078f83b3c',
              'alondrad': '927200dd1af0877c48b9fb480d1b8125056e1a507ad23b97c468aa56aa309cab'},
    '0.1.9': {'commit': 'f6744998a452460d5aaedca926dc04fa4c4a86b7',
              'formula': 'ce0ec23624cc81057d2a4749ffda5450dada492f',
              'archive': 'd929c5653dae0d4528c12dbd9fdebd2b8fb22c33bfd6a2e7eb7145b606e0812f',
              'alondra': '9135ec7f42a2e30a783071f1fba788f7f35844c98df70fb19fbb6e2f5b18b1ca',
              'alondrad': 'cc64ff614dec3bd274aec2bed6a8af4dbf00a57cb9bd3772c251fb6584944070'},
}
CHECKS = ['public_018_install', 'public_019_brew_upgrade', '019_same_home_persistence',
          '018_rejects_019_copy_without_original_mutation', '018_snapshot_restore',
          'uninstall_package_preserves_user_data', 'owned_process_and_port_quiescent']


def publish(path, value):
    temporary = path.with_suffix('.next')
    with temporary.open('x') as stream:
        json.dump(value, stream, indent=2); stream.write('\n'); stream.flush(); os.fsync(stream.fileno())
    temporary.replace(path)
    directory_fd = os.open(path.parent, os.O_RDONLY | os.O_NOFOLLOW)
    try: os.fsync(directory_fd)
    finally: os.close(directory_fd)


def finish_result(path, result):
    """Never expose PASS while guest data is still awaiting its final sync."""
    intended_status = result['status']
    result['status'] = 'finalizing'
    publish(path, result)
    try:
        subprocess.run(['/bin/sync'], check=True, timeout=30, stdin=subprocess.DEVNULL,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except (OSError, subprocess.SubprocessError) as error:
        result.update(status='failed', durability_error=type(error).__name__)
    else:
        result['status'] = intended_status
    publish(path, result)


def state_manifest(root):
    """All durable bytes, rejecting links; stopped Unix sockets are not data."""
    result = {}
    for path in sorted(root.rglob('*')):
        mode = path.lstat().st_mode
        cli.require(not stat.S_ISLNK(mode), 'state contains a symlink')
        if stat.S_ISDIR(mode) or stat.S_ISSOCK(mode): continue
        cli.require(stat.S_ISREG(mode), 'state contains unsupported file')
        result[str(path.relative_to(root))] = cli.digest(path)
    return result


def snapshot(source, target):
    """Called only after owned daemon quiescence; SQLite backup includes WAL."""
    target.mkdir(mode=0o700)
    for path in sorted(source.rglob('*')):
        relative = path.relative_to(source); destination = target / relative
        mode = path.lstat().st_mode
        cli.require(not stat.S_ISLNK(mode), 'snapshot source contains link')
        if stat.S_ISDIR(mode): destination.mkdir(mode=0o700); continue
        if stat.S_ISSOCK(mode): continue
        cli.require(stat.S_ISREG(mode), 'snapshot source is not regular')
        if path.name.endswith(('-wal', '-shm')):
            cli.require(path.with_name(path.name.rsplit('-', 1)[0]).is_file(), 'orphan SQLite sidecar')
            continue
        if path.suffix == '.db':
            with closing(sqlite3.connect(path.as_uri() + '?mode=ro', uri=True)) as original:
                with closing(sqlite3.connect(destination)) as copied:
                    original.backup(copied)
                    cli.require(copied.execute('PRAGMA integrity_check').fetchone() == ('ok',), 'invalid SQLite backup')
        else: shutil.copyfile(path, destination)
        destination.chmod(0o600)
    return state_manifest(target)


def reject_unsupported_downgrade(code, output):
    cli.require(code != 0 and re.search(r"unknown variant [`'\"]splits_released[`'\"]", output),
                '018 audit did not specifically reject the new 019 event')


def retire_stale_socket(state):
    """Only two owned fixture paths; never relax the evidence collector."""
    allowed = {cli.ROOT / 'project/cli-home/.alondra', cli.ROOT / 'project/post-upgrade-state'}
    cli.require(state in allowed, 'socket retirement outside lifecycle fixture')
    for parent in (state, *state.parents):
        if parent == cli.ROOT: break
        cli.require(not parent.is_symlink(), 'socket parent is symlink')
    path = state / 'hooks.sock'
    try: before = path.lstat()
    except FileNotFoundError: return None
    cli.require(stat.S_ISSOCK(before.st_mode) and before.st_uid == os.getuid()
                and stat.S_IMODE(before.st_mode) == 0o600 and before.st_nlink == 1,
                'socket retirement requires owned mode0600 Unix socket')
    fd = os.open(state / 'service-home.lock', os.O_RDONLY | os.O_NOFOLLOW)
    try:
        lock = os.fstat(fd)
        cli.require(stat.S_ISREG(lock.st_mode) and lock.st_uid == os.getuid()
                    and lock.st_nlink == 1 and stat.S_IMODE(lock.st_mode) == 0o600, 'unsafe home lock')
        fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        hashes = state_manifest(state)
        with socket.socket(socket.AF_UNIX) as probe:
            probe.settimeout(1)
            code = probe.connect_ex(str(path))
        cli.require(code == errno.ECONNREFUSED, 'socket not positively quiescent')
        current = path.lstat()
        identity = lambda value: (value.st_dev, value.st_ino, value.st_mode, value.st_uid, value.st_nlink)
        cli.require(identity(current) == identity(before), 'socket changed before unlink')
        metadata = {'path': str(path.relative_to(cli.ROOT)), 'device': before.st_dev,
                    'inode': before.st_ino, 'mode': oct(before.st_mode), 'uid': before.st_uid,
                    'connect_errno': code, 'exclusive_home_lock': True, 'durable_hashes': hashes}
        audits = cli.ROOT / 'project/socket-retirements'
        audits.mkdir(mode=0o700, exist_ok=True)
        audit_stat = audits.lstat()
        cli.require(stat.S_ISDIR(audit_stat.st_mode) and audit_stat.st_uid == os.getuid()
                    and stat.S_IMODE(audit_stat.st_mode) == 0o700, 'unsafe retirement audit directory')
        receipt = audits / (uuid.uuid4().hex + '.json')
        metadata['status'] = 'prepared'
        publish(receipt, metadata)
        directory_fd = os.open(audits, os.O_RDONLY | os.O_NOFOLLOW)
        try: os.fsync(directory_fd)
        finally: os.close(directory_fd)
        # Receipt is durable before mutation; a failed/interrupted unlink keeps it.
        cli.require(identity(path.lstat()) == identity(before), 'socket changed after audit')
        path.unlink()
        state_fd = os.open(state, os.O_RDONLY | os.O_NOFOLLOW)
        try: os.fsync(state_fd)
        finally: os.close(state_fd)
        cli.require(not path.exists() and not path.is_symlink() and state_manifest(state) == hashes,
                    'socket retirement changed durable state')
        metadata['status'] = 'retired'
        metadata['durable_hashes_after'] = state_manifest(state)
        publish(receipt, metadata)
        directory_fd = os.open(audits, os.O_RDONLY | os.O_NOFOLLOW)
        try: os.fsync(directory_fd)
        finally: os.close(directory_fd)
        return metadata
    finally: os.close(fd)


def run():
    os.umask(0o077)
    cli.require(Path.cwd() == cli.ROOT and sys.platform == 'darwin', 'owned Mac guest cwd required')
    cli.require(subprocess.check_output(['/usr/sbin/sysctl', '-n', 'hw.model'], text=True).startswith('VirtualMac'), 'disposable VM required')
    owner = json.loads((cli.ROOT / 'ownership.json').read_text())
    cli.require(re.fullmatch('alondra-gate-[a-f0-9]{32}', owner['vm']) and re.fullmatch('[a-f0-9]{32}', owner['owner_token']), 'missing clone ownership')
    project = cli.ROOT / 'project'; project.mkdir(mode=0o700, exist_ok=True)
    logs = project / 'cli-logs'; logs.mkdir(mode=0o700)
    home = project / 'cli-home'; home.mkdir(mode=0o700)
    state = home / '.alondra'; state.mkdir(mode=0o700)
    (home / 'Projects').mkdir(); (home / '.claude').mkdir()
    # Durable synthetic marker is preserved through the exact same state directory.
    (state / 'lifecycle-marker.json').write_text(json.dumps({'run': owner['owner_token']}))
    (state / 'config.toml').write_text('device_expiry_days = 37\n')
    brew = '/opt/homebrew/bin/brew'; tap = Path('/opt/homebrew/Library/Taps/bitomule/homebrew-tap')
    prefix = Path('/opt/homebrew/opt/alondra')
    base_env = {**os.environ, 'HOMEBREW_NO_AUTO_UPDATE': '1', 'HOMEBREW_NO_ANALYTICS': '1',
                'HOMEBREW_NO_INSTALL_CLEANUP': '1'}
    # Disabling API installation forces a full homebrew/core clone even for
    # this third-party formula (attempt1). Its own commit/blob remain pinned.
    base_env.pop('HOMEBREW_NO_INSTALL_FROM_API', None)
    with socket.socket() as reservation:
        reservation.bind(('127.0.0.1', 0)); port = reservation.getsockname()[1]
    env = {'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home),
           'USER': 'admin', 'LOGNAME': 'admin', 'ALONDRA_LANG': 'en', 'TMPDIR': str(home),
           'ALONDRA_HOME': str(state), 'CLAUDE_HOME': str(home / '.claude'), 'ALONDRA_PORT': str(port),
           'ALONDRA_LISTEN': '127.0.0.1', 'ALONDRA_WAKE': '0', 'ALONDRA_LEGACY_BRIDGE': '0',
           'ALONDRA_RESUME_DEAD_NODES': '0'}
    result = {'schema': 1, 'profile': 'cli-public-package-lifecycle', 'version': '0.1.9',
              'status': 'failed', 'checks': [], 'phases': [], 'releases': RELEASES,
              'limits': ['ARM64 guest only', 'no login/setup/launchd/Claude',
                         'rollback restores pre-upgrade snapshot; later changes retained separately, not replayed',
                         'no general downgrade or migration compatibility claim']}
    process = None; daemon_log = None; counter = 0
    def command(argv, name, timeout=90, execution_env=base_env):
        nonlocal counter
        counter += 1
        return cli.command(argv, execution_env, logs / ('%03d-%s.log' % (counter, name)), timeout)
    def phase(name, **evidence):
        result['phases'].append({'phase': name, **evidence})
        publish(project / 'cli-result.json', result)
    def checkout(version):
        record = RELEASES[version]
        cli.require(not command(['git', '-C', str(tap), 'status', '--porcelain'], 'tap-status').strip(), 'tap changed unexpectedly')
        command(['git', '-C', str(tap), 'checkout', '--detach', record['commit']], 'tap-checkout')
        cli.require(command(['git', '-C', str(tap), 'rev-parse', 'HEAD'], 'tap-head').strip() == record['commit'], 'tap commit mismatch')
        cli.require(command(['git', '-C', str(tap), 'rev-parse', 'HEAD:Formula/alondra.rb'], 'formula-blob').strip() == record['formula'], 'formula blob mismatch')
    def verify(version, name):
        cli.require(prefix.resolve() == Path('/opt/homebrew/Cellar/alondra') / version, 'Brew opt selected wrong keg')
        hashes = {}
        for binary in ('alondra', 'alondrad'):
            path = prefix / 'bin' / binary
            hashes[binary] = cli.digest(path)
            cli.require(hashes[binary] == RELEASES[version][binary], 'published executable hash mismatch')
            command(['/usr/bin/codesign', '--verify', '--strict', str(path)], name + '-verify')
            signature = command(['/usr/bin/codesign', '-dvv', str(path)], name + '-signature')
            cli.require('TeamIdentifier=' + cli.TEAM in signature and 'Authority=Developer ID Application:' in signature, 'unexpected signer')
        cli.require({str(p.relative_to(prefix / 'libexec/alondra')) for p in (prefix / 'libexec/alondra').rglob('*') if p.is_file()} == cli.ASSETS, 'asset allowlist mismatch')
        command([brew, 'test', 'alondra'], name + '-brew-test', 120)
        return hashes
    def start(version, name):
        nonlocal process, daemon_log
        cli.require(process is None, 'previous daemon still owned')
        daemon_log = (logs / (name + '-daemon.log')).open('xb')
        def file_bound(): resource.setrlimit(resource.RLIMIT_FSIZE, (cli.MAX_LOG, cli.MAX_LOG))
        process = subprocess.Popen([str(prefix / 'bin/alondrad')], env=env, stdin=subprocess.DEVNULL,
                                   stdout=daemon_log, stderr=subprocess.STDOUT, preexec_fn=file_bound)
        legacy_readiness(process, port, version, state)
        cli.require(command([str(prefix / 'bin/alondra'), 'config', '--get', 'device_expiry_days'],
                            name + '-config', execution_env=env).strip() == '37', 'persisted config was not applied')
        return process.pid
    def stop():
        nonlocal process, daemon_log
        pid = process.pid if process else None
        cli.stop(process)
        cli.require(process is None or process.poll() is not None, 'owned daemon did not stop')
        process = None
        if daemon_log is not None: daemon_log.close(); daemon_log = None
        with socket.socket() as probe:
            cli.require(probe.connect_ex(('127.0.0.1', port)) != 0, 'local daemon listener remains')
        retired = retire_stale_socket(state)
        if retired is not None: result.setdefault('retired_sockets', []).append(retired)
        return pid
    def schedules(): return cli.request(port, '/v1/schedules')['schedules']
    def create_schedule(title):
        payload = {'title': title, 'text': 'Synthetic lifecycle fixture; never execute',
                   'frequency': 'daily', 'hour': 0, 'minute': 0, 'enabled': False}
        request = urllib.request.Request('http://127.0.0.1:%s/v1/schedules' % port,
            data=json.dumps(payload).encode(), headers={'Content-Type': 'application/json'}, method='POST')
        with urllib.request.build_opener(urllib.request.ProxyHandler({})).open(request, timeout=5) as response:
            cli.require(response.status == 201, 'schedule create failed')
            value = json.loads(response.read(65536))
        cli.require(value['enabled'] is False and value['next_run'] is None, 'fixture schedule could execute')
        return value
    def audit(name):
        return command([str(prefix / 'bin/alondra'), 'state', 'audit'], name, execution_env=env).strip()
    try:
        cli.require(not Path('/Users/admin/.alondra').exists() and not prefix.exists()
                    and not Path('/opt/homebrew/Cellar/alondra').exists(), 'clean Alondra guest required')
        cli.require(not tap.exists(), 'refuse to modify a pre-existing tap checkout')
        cli.require(not Path('/Users/admin/Library/LaunchAgents/com.bitomule.alondra.plist').exists(), 'existing service plist')
        command([brew, 'tap', 'bitomule/tap'], 'tap', 120)
        for record in RELEASES.values():
            command(['git', '-C', str(tap), 'fetch', '--no-tags', 'origin', record['commit']], 'fetch-formula-commit', 90)
        for version in RELEASES:
            checkout(version)
            command([brew, 'fetch', '--force', 'bitomule/tap/alondra'], 'fetch-' + version, 180)
            cached = Path(command([brew, '--cache', 'bitomule/tap/alondra'], 'cache-' + version).strip())
            cli.require(cached.is_file() and cli.digest(cached) == RELEASES[version]['archive'], 'cached archive mismatch')
        checkout('0.1.8')
        command([brew, 'install', 'bitomule/tap/alondra'], 'install-018', 180)
        phase('install-018', binary_hashes=verify('0.1.8', '018'))
        result['checks'].append('public_018_install')
        command(['/usr/bin/sudo', '-n', '/sbin/ifconfig', 'en0', 'down'], 'network-down', 15)
        cli.check_guest_network_disabled(command(['/sbin/ifconfig', 'en0'], 'network-status'))
        pid = start('0.1.8', '018')
        seeded = create_schedule('Pre-upgrade schedule')
        before_seq = cli.request(port, '/v1/overview')['seq']
        before_rows = schedules(); stop()
        before_audit = audit('018-audit')
        backup = project / 'pre-upgrade-state'; hashes = snapshot(state, backup)
        phase('pre-upgrade-snapshot', pid_stopped=pid, seq=before_seq, schedules=before_rows,
              audit=before_audit, hashes=hashes)
        checkout('0.1.9')
        command([brew, 'upgrade', 'bitomule/tap/alondra'], 'upgrade-019', 180)
        phase('upgrade-019', binary_hashes=verify('0.1.9', '019'))
        result['checks'].append('public_019_brew_upgrade')
        pid = start('0.1.9', '019')
        cli.require(schedules() == before_rows, 'upgrade lost or changed persisted schedule')
        cli.require((state / 'lifecycle-marker.json').read_bytes() == (backup / 'lifecycle-marker.json').read_bytes(), 'upgrade changed marker')
        cli.require((state / 'config.toml').read_bytes() == (backup / 'config.toml').read_bytes(), 'upgrade changed configuration')
        later = create_schedule('Post-upgrade schedule; preserved outside rollback')
        upgraded_seq = cli.request(port, '/v1/overview')['seq']
        cli.require(upgraded_seq > before_seq, '019 did not record its new bootstrap event')
        upgraded_rows = schedules(); result['tui'] = cli.tui(prefix / 'bin/alondra', env, 'Local'); stop()
        upgraded_audit = audit('019-audit')
        with closing(sqlite3.connect((state / 'alondra.db').as_uri() + '?mode=ro', uri=True)) as db:
            cli.require(db.execute("SELECT COUNT(*) FROM event_log WHERE kind='splits_released'").fetchone() == (1,), 'missing new event for downgrade oracle')
            cli.require(db.execute('SELECT COUNT(*) FROM snapshots').fetchone() == (0,), 'unexpected compaction hides downgrade event')
        preserved = project / 'post-upgrade-state'
        state.rename(preserved)  # Original 019 state survives; never run 018 against it.
        negative = project / 'downgrade-audit-copy'; snapshot(preserved, negative)
        original019 = state_manifest(preserved)
        old = Path('/opt/homebrew/Cellar/alondra/0.1.8/bin/alondra')
        cli.require(cli.digest(old) == RELEASES['0.1.8']['alondra'], 'old keg not retained')
        probe = subprocess.run([str(old), 'state', 'audit'], env={**env, 'ALONDRA_HOME': str(negative)},
                               stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=20)
        cli.require(len(probe.stdout) <= cli.MAX_LOG, 'oversized downgrade diagnostic')
        (logs / 'negative-018-audit-019.log').write_bytes(probe.stdout)
        reject_unsupported_downgrade(probe.returncode, probe.stdout.decode(errors='replace'))
        cli.require(state_manifest(preserved) == original019, 'negative probe changed original 019 state')
        phase('019-preserved-and-downgrade-rejected', pid_stopped=pid, seq=upgraded_seq, audit=upgraded_audit,
              schedules=upgraded_rows, hashes=original019, rejection_exit=probe.returncode)
        result['checks'].extend(['019_same_home_persistence', '018_rejects_019_copy_without_original_mutation'])
        command([brew, 'uninstall', '--force', 'alondra'], 'rollback-remove-kegs', 90)
        checkout('0.1.8'); command([brew, 'install', 'bitomule/tap/alondra'], 'rollback-install-018', 180)
        rollback_hashes = verify('0.1.8', 'rollback-018')
        shutil.copytree(backup, state)
        cli.require(state_manifest(state) == hashes, 'restored state differs from pre-upgrade snapshot')
        cli.require(audit('restored-018-audit') == before_audit, 'restored snapshot audit differs')
        pid = start('0.1.8', 'restored-018')
        cli.require(schedules() == before_rows and seeded['id'] != later['id'], 'rollback data boundary incorrect')
        cli.require(cli.request(port, '/v1/overview')['seq'] == before_seq, 'rollback sequence differs')
        stop(); cli.require(audit('restored-018-stopped-audit') == before_audit, 'restored runtime changed state')
        cli.require(state_manifest(preserved) == original019, 'preserved 019 state changed')
        phase('rollback-018-pre-upgrade-snapshot', pid_stopped=pid, seq=before_seq, binary_hashes=rollback_hashes,
              restored_schedules=before_rows, preserved_later_schedule=later['id'])
        result['checks'].append('018_snapshot_restore')
        user_data = state_manifest(state)
        command([brew, 'uninstall', '--force', 'alondra'], 'uninstall-final', 90)
        for path in [prefix, Path('/opt/homebrew/Cellar/alondra'), Path('/opt/homebrew/bin/alondra'), Path('/opt/homebrew/bin/alondrad')]:
            cli.require(not path.exists() and not path.is_symlink(), 'installed package path remains')
        cli.require(state_manifest(state) == user_data and state_manifest(preserved) == original019, 'uninstall changed user data')
        stop()
        cli.require(not Path('/Users/admin/Library/LaunchAgents/com.bitomule.alondra.plist').exists(), 'unexpected launchd service')
        phase('uninstall-final', installed_paths_absent=True, state_hashes=user_data, original019_hashes=original019)
        result['checks'].extend(['uninstall_package_preserves_user_data', 'owned_process_and_port_quiescent'])
        result['status'] = 'passed'
    except Exception as error:
        result['failure'] = {'kind': type(error).__name__, 'detail': str(error)[:400]}
    finally:
        for sig in (signal.SIGINT, signal.SIGTERM): signal.signal(sig, signal.SIG_IGN)
        try: stop(); result['cleanup'] = True
        except Exception as error:
            result.update(status='failed', cleanup=False, cleanup_error=type(error).__name__)
        finish_result(project / 'cli-result.json', result)
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    def interrupted(signum, _): raise RuntimeError('interrupted lifecycle ' + str(signum))
    signal.signal(signal.SIGINT, interrupted); signal.signal(signal.SIGTERM, interrupted)
    sys.exit(run())
