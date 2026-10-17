"""Gives one sandbox Mac the SDK backend (tools/e2e-sandbox.sh with SDK=1).

Usage: e2e_sdk_backend.py <sandbox dir> <Mac home> <node> <persistent.cjs> <native archive>

Copies the pinned Node and the host bundle, extracts the pinned native Claude from its npm archive
(size and SHA256 checked), starts the local model double, writes the Seatbelt profile the whole host
runs under and the Mac's sdk-backend.json. Claude gets a synthetic HOME, configuration and
temporary folder inside the sandbox and the public fixture key: never the user's login, Keychain
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

REPO = Path(__file__).resolve().parent.parent


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


def profile(runtime, home, tmp, state, work, port):
    reads = [runtime, home, tmp, state, work]
    parents = sorted({str(parent) for path in reads for parent in path.parents})
    return '\n'.join([
        '(version 1)', '(deny default)', '(deny mach-lookup)',
        '(allow process-fork)', '(allow signal (target same-sandbox))',
        '(allow process-info* (target self))', '(allow sysctl-read)',
        '(allow process-exec (literal ' + quote(runtime / 'node') + ') (literal ' + quote(runtime / 'claude') + '))',
        '(allow file-read-data (literal "/"))',
        '(allow file-read-metadata ' + ' '.join('(literal ' + quote(parent) + ')' for parent in parents) + ')',
        '(allow file-read* (subpath ' + quote(runtime) + ') (subpath ' + quote(work) + ') (subpath "/System/Library") (subpath "/usr/lib") (subpath "/usr/share/icu") (subpath "/private/var/db/dyld") (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom"))',
        '(allow file-read* file-write* (subpath ' + quote(home) + ') (subpath ' + quote(tmp) + ') (subpath ' + quote(state) + ') (literal "/dev/null"))',
        '(allow network-bind network-inbound (local unix-socket (subpath ' + quote(state) + ')))',
        '(allow network-outbound (remote ip "localhost:' + str(port) + '"))',
    ]) + '\n'


def main():
    sandbox, mac, node, host, archive = (Path(value).resolve() for value in sys.argv[1:6])
    root = sandbox / 'sdk'
    runtime, home, tmp = root / 'runtime', root / 'home', root / 'tmp'
    for folder in [runtime, home / 'config', tmp]:
        folder.mkdir(mode=0o700, parents=True)
    shutil.copyfile(node, runtime / 'node')
    (runtime / 'node').chmod(0o700)
    shutil.copyfile(host, runtime / 'persistent.cjs')
    (runtime / 'persistent.cjs').chmod(0o400)
    native_sha256 = fixture().native_from_archive(archive, runtime / 'claude')
    model = subprocess.Popen([sys.executable, '-I', str(REPO / 'tests/native/claude/model_double.py'), str(root / 'model-requests.jsonl')],
                             stdout=subprocess.PIPE, stdin=subprocess.DEVNULL, stderr=open(root / 'model.log', 'ab'), start_new_session=True)
    port = int(model.stdout.readline())
    model.stdout.close()
    (root / 'model.pid').write_text(str(model.pid))
    state = Path(os.path.realpath('/tmp')) / ('as-sdk-' + checksum(mac / 'service.toml')[:8])
    state.mkdir(mode=0o700)
    (root / 'state-root').write_text(str(state))
    work = Path(os.path.realpath(sandbox / 'repo'))
    (root / 'host.sb').write_text(profile(runtime, home, tmp, state, work, port))
    synthetic = {'HOME': str(home), 'PATH': '/usr/bin:/bin'}
    native = dict(synthetic, CLAUDE_CONFIG_DIR=str(home / 'config'), TMPDIR=str(tmp), CLAUDE_CODE_TMPDIR=str(tmp), LANG='C.UTF-8', TZ='UTC',
                  ANTHROPIC_API_KEY='sk-ant-public-loopback-fixture-only', ANTHROPIC_BASE_URL='http://127.0.0.1:' + str(port),
                  CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC='1', DISABLE_AUTOUPDATER='1')
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
