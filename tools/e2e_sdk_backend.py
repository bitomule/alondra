"""Gives one sandbox Mac the SDK backend (tools/e2e-sandbox.sh).

Usage: e2e_sdk_backend.py <sandbox dir> <Mac home> <node> <persistent.cjs> <native archive>
       e2e_sdk_backend.py --fake <sandbox dir> <Mac home> <node> <persistent.cjs> <fake claude>

With --fake (every sandbox Mac by default) the native Claude is the sandbox's scripted fake
(tools/sandbox/src/fake/native.rs): the pinned Node and the host bundle, no model, no Seatbelt and a
synthetic HOME. Otherwise (SDK=1):

Copies the pinned Node and the host bundle, extracts the pinned native Claude from its npm archive
(size and SHA256 checked), starts the local model double, writes the Seatbelt profile the whole host
runs under and the Mac's sdk-backend.json. The Mac's orchestrator runs through the same host: the
profile also lets its turn start Alondra's MCP server and reach the Mac's daemon. Claude gets a synthetic HOME, whose
~/.claude is its default profile (no CLAUDE_CONFIG_DIR), and a temporary folder inside the sandbox and the public fixture key: never the user's login, Keychain
or ~/.claude.json, and no network beyond the model double on loopback.
"""
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

REPO = Path(__file__).resolve().parent.parent
# The git of the Command Line Tools itself, not the /usr/bin shim that goes through xcrun.
TOOLS = Path('/Library/Developer/CommandLineTools')
GIT = TOOLS / 'usr/bin/git'
GIT_CORE = TOOLS / 'usr/libexec/git-core'


def fixture():
    spec = importlib.util.spec_from_file_location('native_fixture', REPO / 'tests/native/claude/run.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def checksum(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1048576), b''):
            digest.update(chunk)
    return digest.hexdigest()


def quote(value):
    return json.dumps(str(value))


def profile(runtime, home, tmp, state, work, port, orchestrator):
    """`orchestrator` is what the Mac's orchestrator turn needs besides a task's: its folder, the
    plugins and the alondra CLI their MCP server runs, the Mac's home and its daemon's port."""
    folder, plugins, cli, mac, daemon = orchestrator
    reads = [runtime, home, tmp, state, work, folder, cli, mac, GIT] + plugins
    parents = sorted({str(parent) for path in reads for parent in path.parents})
    return '\n'.join([
        '(version 1)', '(deny default)', '(deny mach-lookup)',
        '(allow process-fork)', '(allow signal (target same-sandbox))',
        '(allow process-info* (target self))', '(allow sysctl-read)',
        '(allow process-exec (literal ' + quote(runtime / 'node') + ') (literal ' + quote(runtime / 'claude') + ') (literal "/bin/sh") (literal "/bin/bash") (literal "/bin/sleep") (literal "/usr/bin/dirname") (literal ' + quote(GIT) + ') (subpath ' + quote(GIT_CORE) + ') (literal ' + quote(cli) + ') ' + ' '.join('(literal ' + quote(plugin / 'bin' / 'alondra') + ')' for plugin in plugins) + ')',
        '(allow file-read-data (literal "/"))',
        '(allow file-read-metadata ' + ' '.join('(literal ' + quote(parent) + ')' for parent in parents) + ')',
        '(allow file-read* (subpath ' + quote(runtime) + ') (subpath ' + quote(work) + ') (subpath "/System/Library") (subpath "/usr/lib") (subpath "/usr/share/icu") (subpath "/private/var/db/dyld") (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom") (literal "/bin/sh") (literal "/bin/bash") (literal "/bin/sleep") (literal "/private/var/select/sh") (literal "/usr/bin/dirname") (literal ' + quote(cli) + ') (subpath ' + quote(mac) + ') ' + ' '.join('(subpath ' + quote(plugin) + ')' for plugin in plugins) + ')',
        '(allow file-read* (subpath ' + quote(TOOLS) + '))',
        # git looks for the system's configuration before the repository's.
        '(allow file-read-metadata (literal "/etc") (literal "/private/etc") (literal "/private/etc/gitconfig"))',
        # The task's worktrees and its subagents' are made with git in the sandbox's own repository.
        '(allow file-read* file-write* (subpath ' + quote(folder) + ') (subpath ' + quote(work) + '))',
        '(allow file-read* file-write* (subpath ' + quote(home) + ') (subpath ' + quote(tmp) + ') (subpath ' + quote(state) + ') (literal "/dev/null"))',
        '(allow network-bind network-inbound (local unix-socket (subpath ' + quote(state) + ')))',
        '(allow network-outbound (remote ip "localhost:' + str(port) + '") (remote ip "localhost:' + str(daemon) + '"))',
    ]) + '\n'


def runtime_of(sandbox, node, host, exist_ok):
    """The sandbox's copies of the pinned Node and the host bundle, with Claude's HOME and TMPDIR."""
    root = sandbox / 'sdk'
    runtime, home, tmp = root / 'runtime', root / 'home', root / 'tmp'
    for folder in [runtime, home, tmp]:
        folder.mkdir(mode=0o700, parents=True, exist_ok=exist_ok)
    if not (runtime / 'node').exists():
        shutil.copyfile(node, runtime / 'node')
        (runtime / 'node').chmod(0o700)
    if not (runtime / 'persistent.cjs').exists():
        shutil.copyfile(host, runtime / 'persistent.cjs')
        (runtime / 'persistent.cjs').chmod(0o400)
    return root, runtime, home, tmp


def fake(sandbox, mac, node, host, native):
    _, runtime, home, tmp = runtime_of(sandbox, node, host, exist_ok=True)
    # Short enough for the host's Unix socket: <state root>/<execution UUID>/s.
    state = Path(tempfile.mkdtemp(prefix='as-sdk-', dir=os.path.realpath('/tmp')))
    (mac / 'sdk-state-root').write_text(str(state))
    synthetic = {'HOME': str(home), 'PATH': '/usr/bin:/bin'}
    backend = {
        'node': str(runtime / 'node'), 'node_sha256': checksum(runtime / 'node'),
        'host': str(runtime / 'persistent.cjs'), 'host_sha256': checksum(runtime / 'persistent.cjs'),
        'native': str(native), 'native_sha256': checksum(native),
        'host_environment': synthetic,
        'native_environment': dict(synthetic, PATH='/usr/bin:/bin:/usr/sbin:/sbin', TMPDIR=str(tmp), LANG='C.UTF-8', TZ='UTC'),
        'state_root': str(state), 'io_timeout_seconds': 120,
    }
    (mac / 'sdk-backend.json').write_text(json.dumps(backend, indent=2) + '\n')


def main():
    if sys.argv[1] == '--fake':
        fake(*(Path(value).resolve() for value in sys.argv[2:7]))
        return
    sandbox, mac, node, host, archive = (Path(value).resolve() for value in sys.argv[1:6])
    root, runtime, home, tmp = runtime_of(sandbox, node, host, exist_ok=False)
    native_sha256 = fixture().native_from_archive(archive, runtime / 'claude')
    model = subprocess.Popen([sys.executable, '-I', str(REPO / 'tests/native/claude/model_double.py'), str(root / 'model-requests.jsonl'), str(sandbox / 'fakes' / 'scripts')],
                             stdout=subprocess.PIPE, stdin=subprocess.DEVNULL, stderr=open(root / 'model.log', 'ab'), start_new_session=True)
    port = int(model.stdout.readline())
    model.stdout.close()
    (root / 'model.pid').write_text(str(model.pid))
    state = Path(os.path.realpath('/tmp')) / ('as-sdk-' + checksum(mac / 'service.toml')[:8])
    state.mkdir(mode=0o700)
    (root / 'state-root').write_text(str(state))
    work = Path(os.path.realpath(sandbox / 'repo'))
    daemon = next(line.split('=', 1)[1].strip() for line in (mac / 'config.toml').read_text().splitlines() if line.startswith('port'))
    folder = mac / 'runtime' / 'orchestrator'
    folder.mkdir(parents=True, exist_ok=True)
    plugins = [Path(os.path.realpath(sandbox / name)) for name in ('plugin', 'plugin-orchestrator')] + [Path(os.path.realpath(mac / 'plugin'))]
    orchestrator = (Path(os.path.realpath(folder)), plugins, Path(os.path.realpath(sandbox / 'bin' / 'alondra')), Path(os.path.realpath(mac)), daemon)
    (root / 'host.sb').write_text(profile(runtime, home, tmp, state, work, port, orchestrator))
    synthetic = {'HOME': str(home), 'PATH': '/usr/bin:/bin'}
    native = dict(synthetic, PATH=str(GIT.parent) + ':/usr/bin:/bin', TMPDIR=str(tmp), CLAUDE_CODE_TMPDIR=str(tmp), LANG='C.UTF-8', TZ='UTC',
                  ANTHROPIC_API_KEY='sk-ant-public-loopback-fixture-only', ANTHROPIC_BASE_URL='http://127.0.0.1:' + str(port),
                  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', DISABLE_AUTOUPDATER='1', CLAUDE_CODE_MAX_RETRIES='1')
    backend = {
        'node': str(runtime / 'node'), 'node_sha256': checksum(runtime / 'node'),
        'host': str(runtime / 'persistent.cjs'), 'host_sha256': checksum(runtime / 'persistent.cjs'),
        'native': str(runtime / 'claude'), 'native_sha256': native_sha256,
        'host_environment': synthetic, 'native_environment': native,
        'confinement': str(root / 'host.sb'), 'state_root': str(state), 'io_timeout_seconds': 120,
    }
    (mac / 'sdk-backend.json').write_text(json.dumps(backend, indent=2) + '\n')


if __name__ == '__main__':
    main()
