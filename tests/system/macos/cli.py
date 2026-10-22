#!/usr/bin/env python3
"""Public package/local-runtime gate. No login, pairing or Claude substitute."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import pty
import re
import resource
import select
import signal
import socket
import subprocess
import sys
import termios
import time
import struct
import sqlite3
import urllib.request
from contextlib import closing

ROOT = Path('/Users/admin/alondra-gate')
TEAM = '658YBSK6MG'
MAX_LOG = 4 * 1024 * 1024
ASSETS = {'.claude-plugin/marketplace.json', 'plugin/bin/alondra', 'plugin/.mcp.json',
          'plugin/hooks/hooks.json', 'plugin/.claude-plugin/plugin.json', 'plugin/skills/alondra/SKILL.md',
          'plugin-orchestrator/.mcp.json', 'plugin-orchestrator/.claude-plugin/plugin.json',
          'plugin-orchestrator/skills/alondra-orchestrator/SKILL.md', 'plugin-orchestrator/bin/alondra'}


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


def digest(path):
    h = hashlib.sha256()
    with path.open('rb') as stream:
        for block in iter(lambda: stream.read(65536), b''):
            h.update(block)
    return h.hexdigest()


def tree(root):
    result = {}
    for p in sorted(root.rglob('*')):
        require(not p.is_symlink(), 'unexpected fixture symlink')
        result[str(p.relative_to(root))] = digest(p) if p.is_file() else 'directory'
    return result


def stop(process):
    if process is None or process.poll() is not None:
        return
    process.terminate()
    try:
        process.wait(timeout=8)
    except subprocess.TimeoutExpired:
        process.kill()
        process.wait(timeout=5)


def command(argv, env, log, timeout=90):
    """Bounded output and time; never echo captured child output to the host."""
    process = subprocess.Popen(argv, stdin=subprocess.DEVNULL, stdout=subprocess.PIPE,
                               stderr=subprocess.STDOUT, env=env)
    output = bytearray()
    deadline = time.monotonic() + timeout
    try:
        with log.open('xb') as file:
            while True:
                require(time.monotonic() < deadline, 'command timeout: ' + log.name)
                if select.select([process.stdout], [], [], .1)[0]:
                    part = os.read(process.stdout.fileno(), 65536)
                    if not part:
                        break
                    require(len(output) + len(part) <= MAX_LOG, 'command output bound: ' + log.name)
                    output.extend(part)
                    file.write(part)
            require(process.wait(timeout=max(.1, deadline - time.monotonic())) == 0,
                    'command failed: ' + log.name)
        return output.decode('utf-8', errors='replace')
    finally:
        stop(process)
        process.stdout.close()


def request(port, path):
    opener = urllib.request.build_opener(urllib.request.ProxyHandler({}))
    with opener.open(f'http://127.0.0.1:{port}{path}', timeout=2) as response:
        body = response.read(1024 * 1024 + 1)
        require(response.status == 200 and len(body) <= 1024 * 1024, 'invalid local response')
        return json.loads(body)


def readiness(process, port, version):
    deadline = time.monotonic() + 20
    while time.monotonic() < deadline:
        require(process.poll() is None, 'daemon exited before readiness')
        try:
            daemon = request(port, '/daemon')
        except (OSError, ValueError):
            time.sleep(.1)
            continue
        require(daemon.get('version') == version, 'daemon version mismatch')
        return daemon
    raise RuntimeError('daemon readiness timeout')


def status_blocks(output):
    """`alondra status` prints the home and tasks views under unindented headings."""
    blocks, current = {}, None
    for line in output.splitlines():
        if line and not line.startswith(' ') and line.endswith(':'):
            current = line[:-1]
            blocks[current] = []
        elif current is not None and line.strip():
            blocks[current].append(line.strip())
    return blocks


def stored_domain(state):
    """Read once the daemon has stopped: no route without a device signature carries the revision."""
    with closing(sqlite3.connect((state / 'domain.db').as_uri() + '?mode=ro', uri=True)) as db:
        (revision,) = db.execute('SELECT revision FROM domain_meta WHERE singleton=1').fetchone()
        (tasks,) = db.execute('SELECT COUNT(*) FROM tasks').fetchone()
        (agents,) = db.execute('SELECT COUNT(*) FROM agents').fetchone()
    return {'revision': revision, 'tasks': tasks, 'agents': agents}


def local_daemon_checks(binary, daemon, env, port, state, version, logs, with_tui, owned):
    """Two starts of the daemon on an empty home: it answers, reads the configuration a finished
    setup leaves, stays empty and writes nothing to the domain. That file is what makes `alondra`
    open the TUI rather than the setup wizard.

    Every daemon started is appended to `owned`, so the caller can still stop it after a signal."""
    def bound_daemon_files():
        # This fixture is empty; limit every file to bound daemon log output too.
        resource.setrlimit(resource.RLIMIT_FSIZE, (MAX_LOG, MAX_LOG))
    state.mkdir(mode=0o700, exist_ok=True)
    (state / 'config.toml').write_text('listen = "127.0.0.1"\n')
    result = {}
    revisions = []
    for cycle in range(2):
        process = None
        with (logs / f'daemon-{cycle}.log').open('xb') as daemon_log:
            try:
                process = subprocess.Popen([str(daemon)], env=env, stdin=subprocess.DEVNULL, stdout=daemon_log,
                                           stderr=subprocess.STDOUT, preexec_fn=bound_daemon_files)
                owned.append(process)
                readiness(process, port, version)
                blocks = status_blocks(command([str(binary), 'status'], env, logs / f'status-{cycle}.log'))
                require(blocks.get('Waiting for You') == [] and 'Tasks' in blocks, 'expected an empty home view')
                require(command([str(binary), 'config', 'get', 'home'], env, logs / f'home-{cycle}.log').strip() == str(state),
                        'CLI home mismatch')
                require(command([str(binary), 'config', 'get', 'port'], env, logs / f'port-{cycle}.log').strip() == str(port),
                        'CLI port mismatch')
                require(command([str(binary), 'config', 'get', 'listen'], env, logs / f'listen-{cycle}.log').strip() == '127.0.0.1',
                        'CLI listen mismatch')
                if with_tui and cycle == 1:
                    result['tui'] = tui(binary, env)
                require(process.poll() is None, 'daemon exited during local checks')
            finally:
                stop(process)
        stored = stored_domain(state)
        require(stored['tasks'] == 0 and stored['agents'] == 0, 'expected an empty domain')
        revisions.append(stored['revision'])
        require(not (state / 'service.toml').exists(), 'unexpected service enrollment')
    require(revisions[0] == revisions[1], 'empty domain changed across restart')
    result['persisted_revision'] = revisions[-1]
    return result


def final_screen(data):
    screen = [[' '] * 160 for _ in range(45)]
    row = col = 0
    tokens = re.findall(r'\x1b\[[0-?]*[ -/]*[@-~]|\x1b[_P\]][^\x1b]*(?:\x1b\\|\x07)|[^\x1b]', data.decode(errors='replace'))
    for token in tokens:
        if token.startswith('\x1b['):
            cmd, params = token[-1], token[2:-1]
            if params.startswith('?'):
                continue
            nums = [int(x) if x else 0 for x in params.split(';')] if all(x.isdigit() or not x for x in params.split(';')) else [0]
            n = nums[0] or 1
            if cmd in ('H', 'f'):
                row = min(44, max(0, n - 1)); col = min(159, max(0, (nums[1] if len(nums) > 1 and nums[1] else 1) - 1))
            elif cmd == 'G': col = min(159, n - 1)
            elif cmd == 'A': row = max(0, row - n)
            elif cmd == 'B': row = min(44, row + n)
            elif cmd == 'C': col = min(159, col + n)
            elif cmd == 'D': col = max(0, col - n)
            elif cmd == 'J' and nums[0] in (2, 3): screen = [[' '] * 160 for _ in range(45)]
            elif cmd == 'K':
                start, end = (0, 160) if nums[0] == 2 else ((0, col + 1) if nums[0] == 1 else (col, 160))
                screen[row][start:end] = [' '] * (end - start)
        elif token.startswith('\x1b'): continue
        elif token == '\r': col = 0
        elif token == '\n': row = min(44, row + 1)
        elif token.isprintable(): screen[row][col] = token; col = min(159, col + 1)
    return '\n'.join(''.join(line) for line in screen)


def tui(binary, env, landmark='Bandeja'):
    master, slave = pty.openpty()
    fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 45, 160, 0, 0))
    def own_terminal():
        os.setsid()
        fcntl.ioctl(0, termios.TIOCSCTTY, 0)
    process = subprocess.Popen([str(binary)], stdin=slave, stdout=slave, stderr=slave,
        env={**env, 'TERM': 'xterm-256color', 'ALONDRA_LANG': 'es'}, preexec_fn=own_terminal)
    os.close(slave)
    data = bytearray()
    cursor = status = 0
    def drain(seconds):
        nonlocal cursor, status
        until = time.monotonic() + seconds
        while process.poll() is None and time.monotonic() < until:
            if select.select([master], [], [], .1)[0]:
                try: part = os.read(master, 65536)
                except OSError: break
                data.extend(part)
                require(len(data) <= 1024 * 1024, 'TUI output bound')
                while cursor < data.count(b'\x1b[6n'):
                    os.write(master, b'\x1b[1;1R'); cursor += 1
                while status < data.count(b'\x1b[5n'):
                    os.write(master, b'\x1b[?1;2c\x1b[6;16;8t\x1b[0n'); status += 1
    try:
        require(os.getsid(process.pid) == process.pid and os.tcgetpgrp(master) == process.pid, 'TUI does not own terminal')
        drain(12)
        screen = final_screen(data)
        loaded = bool(re.search(r'\d+ trabajando', screen.splitlines()[0]))
        offline = any(x in screen for x in ('no responde', 'sin conexión', 'Conectando con alondrad'))
        require(process.poll() is None, 'TUI exited before quit')
        os.write(master, b'q')
        drain(5)
        require(process.wait(timeout=2) == 0, 'TUI q exit failed')
        require(loaded and landmark in screen and not offline and b'\x1b[?1049l' in data, 'TUI did not render its online home')
        return {'online_header': loaded, 'offline': offline, 'exit': 0, 'cursor_replies': cursor,
                'device_status_replies': status, 'bytes_observed': len(data), 'screen_retained': False}
    finally:
        stop(process)
        os.close(master)


def check_plan(plan, prefix):
    """`doctor` without --fix only reports: the service it would install runs the stable Brew opt daemon."""
    report = json.loads(plan)
    install = [check['install'] for check in report['checks'] if check.get('id') == 'service' and check.get('install')]
    require(len(install) == 1, 'doctor did not report the service it would install')
    program = install[0]['program']
    require(program == f'{prefix}/bin/alondrad', 'doctor did not discover stable Brew opt daemon')
    require('/Cellar/' not in program and '.local/bin/alondrad' not in program,
            'doctor proposed a source-checkout or unstable daemon path')


def check_guest_network_disabled(output):
    match = re.search(r'^en0: flags=[0-9a-fA-Fx]+<([^>]*)>', output, re.MULTILINE)
    require(match is not None and 'UP' not in match.group(1).split(','),
            'guest en0 is still up or could not be verified')


def run(version):
    require(re.fullmatch(r'\d+\.\d+\.\d+', version), 'invalid expected version')
    require(Path.cwd() == ROOT and sys.platform == 'darwin', 'owned guest cwd required')
    project = ROOT / 'project'
    project.mkdir(mode=0o700, exist_ok=True)
    logs = project / 'cli-logs'
    logs.mkdir(mode=0o700)
    home = project / 'cli-home'
    home.mkdir(mode=0o700)
    state = home / '.alondra'
    claude = home / '.claude'
    claude.mkdir()
    result = {'schema': 1, 'profile': 'cli-public-package-local-runtime', 'expected_version': version,
              'status': 'failed', 'checks': [], 'limits': ['no authenticated setup', 'no Claude', 'no service pairing', 'no cloud E2E']}
    daemons = []
    base_env = {**os.environ, 'HOMEBREW_NO_AUTO_UPDATE': '1', 'HOMEBREW_NO_ANALYTICS': '1', 'HOMEBREW_NO_INSTALL_CLEANUP': '1'}
    try:
        require(not Path('/Users/admin/.alondra').exists(), 'guest contains Alondra profile')
        brew = '/opt/homebrew/bin/brew'
        require(not Path('/opt/homebrew/opt/alondra').exists(), 'clean install requires absent Alondra')
        command([brew, 'install', 'bitomule/tap/alondra'], base_env, logs / 'brew-install.log', 300)
        prefix = Path(command([brew, '--prefix', 'alondra'], base_env, logs / 'brew-prefix.log').strip())
        require(prefix == Path('/opt/homebrew/opt/alondra'), 'expected stable Brew opt prefix')
        result['binaries'] = {}
        for name in ('alondra', 'alondrad'):
            binary = prefix / 'bin' / name
            command(['/usr/bin/codesign', '--verify', '--strict', str(binary)], base_env, logs / (name + '-verify.log'))
            signature = command(['/usr/bin/codesign', '-dvv', str(binary)], base_env, logs / (name + '-signature.log'))
            require('TeamIdentifier=' + TEAM in signature and 'Authority=Developer ID Application:' in signature, 'unexpected signer')
            result['binaries'][name] = digest(binary)
        binary, daemon = prefix / 'bin/alondra', prefix / 'bin/alondrad'
        version_text = command([brew, 'list', '--versions', 'alondra'], base_env, logs / 'version.log').strip()
        require(version_text == 'alondra ' + version, 'installed package version mismatch')
        result['version'] = version
        command([str(binary), '--help'], base_env, logs / 'help.log')
        assets = prefix / 'libexec/alondra'
        require(not any(p.is_symlink() for p in assets.rglob('*')), 'asset symlink is not packaged')
        require({str(p.relative_to(assets)) for p in assets.rglob('*') if p.is_file()} == ASSETS,
                'packaged asset allowlist mismatch')
        result['assets_sha256'] = tree(assets)
        command([brew, 'test', 'alondra'], base_env, logs / 'brew-test.log', 120)
        result['checks'].append('public_install_signatures_assets_brew_test')
        # Only the owned guest interface changes. Tart exec continues over VSock.
        command(['/usr/bin/sudo', '-n', '/sbin/ifconfig', 'en0', 'down'], base_env, logs / 'network-down.log', 15)
        network = command(['/sbin/ifconfig', 'en0'], base_env, logs / 'network-state.log', 15)
        check_guest_network_disabled(network)
        result['checks'].append('guest_en0_disabled_before_local_checks')
        with socket.socket() as reservation:
            reservation.bind(('127.0.0.1', 0)); port = reservation.getsockname()[1]
        env = {'PATH': '/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin', 'HOME': str(home),
               'USER': 'admin', 'LOGNAME': 'admin', 'ALONDRA_LANG': 'en', 'TMPDIR': str(home), 'ALONDRA_HOME': str(state),
               'CLAUDE_HOME': str(claude), 'ALONDRA_PORT': str(port), 'ALONDRA_LISTEN': '127.0.0.1',
               'ALONDRA_WAKE': '0', 'ALONDRA_LEGACY_BRIDGE': '0', 'ALONDRA_RESUME_DEAD_NODES': '0'}
        before = tree(home)
        plans = []
        for index in range(2):
            plans.append(command([str(binary), 'doctor', '--no-input', '--json', '--only', 'service'],
                                 env, logs / f'doctor-{index}.log'))
            check_plan(plans[-1], prefix)
            require(tree(home) == before, 'doctor mutated fixture state')
        require(plans[0] == plans[1], 'doctor report changed')
        result['checks'].append('doctor_twice_no_profile_writes')
        result.update(local_daemon_checks(binary, daemon, env, port, state, version, logs, True, daemons))
        result['checks'].extend(['isolated_daemon_views_config', 'restart_keeps_empty_domain', 'local_tui_q_exit'])
        require(all(p.stat().st_size <= MAX_LOG for p in logs.iterdir()), 'oversized log')
        result['status'] = 'passed'
    except Exception as error:
        result['failure'] = {'type': type(error).__name__, 'message': str(error)[:400]}
    finally:
        # A second signal cannot skip cleanup or publication of the failed receipt.
        for sig in (signal.SIGINT, signal.SIGTERM): signal.signal(sig, signal.SIG_IGN)
        for process in daemons: stop(process)
        result['cleanup'] = all(process.poll() is not None for process in daemons)
        temp = project / '.cli-result.json.tmp'
        temp.write_text(json.dumps(result, indent=2) + '\n')
        temp.replace(project / 'cli-result.json')
    return 0 if result['status'] == 'passed' else 1


if __name__ == '__main__':
    def interrupted(signum, _frame):
        raise RuntimeError('interrupted by signal ' + str(signum))
    signal.signal(signal.SIGINT, interrupted)
    signal.signal(signal.SIGTERM, interrupted)
    sys.exit(run(sys.argv[1]))
