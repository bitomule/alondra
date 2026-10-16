"""Opt-in real native Claude text roundtrip; only the HTTP/SSE model is a double."""
import ctypes
import ctypes.util
import errno
import hashlib
import http.server
import json
import os
from pathlib import Path, PurePosixPath
import select
import shutil
import signal
import socket
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import uuid

NATIVE_SHA256 = '475517caa611e3318a2f298fa071287ec98955538df78c58f709ecc01e5ae2d3'
NATIVE_SIZE = 249556880
MAX_BODY = 1048576
TOKEN = 'sk-ant-public-loopback-fixture-only'


def checksum(path):
    digest = hashlib.sha256()
    with path.open('rb') as source:
        for chunk in iter(lambda: source.read(1048576), b''):
            digest.update(chunk)
    return digest.hexdigest()


def copy_verified(source, destination, executable=False):
    expected = checksum(source)
    shutil.copyfile(source, destination)
    assert checksum(destination) == expected, 'Copied artifact differs from exact Bazel input'
    destination.chmod(0o700 if executable else 0o400)
    return expected


def native_from_archive(archive, destination):
    with tarfile.open(archive, 'r:gz') as source:
        members = source.getmembers()
        assert len(members) <= 128, 'Unexpected native package layout'
        candidates = [m for m in members if PurePosixPath(m.name).name == 'claude' and m.isfile()]
        assert len(candidates) == 1 and candidates[0].size == NATIVE_SIZE
        digest = hashlib.sha256()
        remaining = NATIVE_SIZE
        with source.extractfile(candidates[0]) as binary, destination.open('xb') as output:
            while remaining:
                chunk = binary.read(min(1048576, remaining))
                assert chunk, 'Truncated native package'
                output.write(chunk)
                digest.update(chunk)
                remaining -= len(chunk)
            assert not binary.read(1)
        assert digest.hexdigest() == NATIVE_SHA256, 'Unmodified pinned native executable required'
    destination.chmod(0o700)
    assert destination.stat().st_size == NATIVE_SIZE
    return digest.hexdigest()


def sandbox_profile(runtime, port):
    quote = lambda value: json.dumps(str(value))
    return '\n'.join([
        '(version 1)', '(deny default)', '(deny mach-lookup)',
        '(allow process-fork)', '(allow signal (target same-sandbox))',
        '(allow process-info* (target self))', '(allow sysctl-read)',
        '(allow process-exec (literal ' + quote(runtime / 'node') + ') (literal ' + quote(runtime / 'claude') + '))',
        '(allow file-read* (subpath ' + quote(runtime) + ') (subpath "/System/Library") (subpath "/usr/lib") (subpath "/private/var/db/dyld") (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom"))',
        '(allow file-write* (subpath ' + quote(runtime / 'home') + ') (subpath ' + quote(runtime / 'tmp') + ') (subpath ' + quote(runtime / 'work') + ') (literal ' + quote(runtime / 'report.json') + ') (literal "/dev/null"))',
        '(allow file-write* ' + ' '.join('(literal ' + quote(runtime / name) + ')' for name in ['isolation.stdout', 'isolation.stderr', 'query.stdout', 'query.stderr']) + ')',
        '(allow network-outbound (remote ip "127.0.0.1:' + str(port) + '"))',
    ])


class Model(http.server.ThreadingHTTPServer):
    daemon_threads = False

    def __init__(self, marker, response):
        super().__init__(('127.0.0.1', 0), Handler)
        self.marker, self.response = marker, response
        self.requests, self.errors = [], []
        self.guard = threading.Lock()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, *_):
        pass

    def reply(self, code, value, content_type='application/json'):
        raw = value if isinstance(value, bytes) else json.dumps(value).encode()
        self.send_response(code)
        self.send_header('content-type', content_type)
        self.send_header('content-length', str(len(raw)))
        self.send_header('request-id', 'req_' + self.server.marker)
        self.send_header('connection', 'close')
        self.end_headers()
        self.wfile.write(raw)
        self.wfile.flush()

    def send_error(self, code, message=None, explain=None):
        self.server.errors.append('Unexpected HTTP request: ' + str(code) + ' ' + str(message))
        super().send_error(code, message, explain)

    def do_GET(self):
        if self.path == '/isolation-ready/' + self.server.marker:
            self.reply(200, {'marker': self.server.marker})
        else:
            self.server.errors.append('Unexpected GET ' + self.path)
            self.reply(404, {'error': 'unexpected fixture route'})

    def do_POST(self):
        try:
            self.connection.settimeout(30)  # Diagnostics for an incomplete request, never a timing assertion.
            length = int(self.headers.get('content-length', '0'))
            assert 0 < length <= MAX_BODY, 'Bounded Messages request required'
            body = json.loads(self.rfile.read(length))
            assert self.headers.get('x-api-key') == TOKEN, 'Only synthetic fixture API key is allowed'
            assert self.headers.get('authorization') is None, 'No saved bearer credentials may be selected'
            path = self.path.split('?', 1)[0]
            assert path in ['/v1/messages', '/v1/messages/count_tokens'], 'Unexpected fixture route'
            with self.server.guard:
                assert len(self.server.requests) < 16, 'Bounded request journal'
                self.server.requests.append({'path': path, 'body': body})
                main = sum(r['path'] == '/v1/messages' for r in self.server.requests)
            if path.endswith('/count_tokens'):
                return self.reply(200, {'input_tokens': 10})
            assert main == 1, 'Exactly one scripted model response; no hidden retry accepted'
            assert body.get('stream') is True
            user = [m for m in body.get('messages', []) if m.get('role') == 'user']
            assert any(self.server.marker in json.dumps(m.get('content')) for m in user), 'Unique prompt marker must reach the actual native HTTP request'
            assert isinstance(body.get('tools'), list) and body['tools'], 'Native preset tools remain enabled'
            usage = {'input_tokens': 10, 'output_tokens': 0}
            message = {'id': 'msg_' + self.server.marker, 'type': 'message', 'role': 'assistant', 'model': body['model'], 'content': [], 'stop_reason': None, 'stop_sequence': None, 'usage': usage}
            events = [
                ('message_start', {'type': 'message_start', 'message': message}),
                ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}),
                ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': self.server.response}}),
                ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
                ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 10}}),
                ('message_stop', {'type': 'message_stop'}),
            ]
            data = ''.join('event: ' + name + '\ndata: ' + json.dumps(event) + '\n\n' for name, event in events).encode()
            self.reply(200, data, 'text/event-stream')
        except Exception as error:
            self.server.errors.append(str(error))
            self.reply(409, {'type': 'error', 'error': {'type': 'invalid_request_error', 'message': str(error)}})


class HarnessInterrupted(RuntimeError):
    pass


class Interruptions:
    def __init__(self):
        self.deferred = True
        self.received = []
        self.previous = {}

    def __enter__(self):
        for number in (signal.SIGINT, signal.SIGTERM):
            self.previous[number] = signal.signal(number, self.handle)
        return self

    def __exit__(self, *_):
        for number, handler in self.previous.items():
            signal.signal(number, handler)

    def handle(self, number, _frame):
        if number not in self.received:
            self.received.append(number)
        if not self.deferred:
            self.deferred = True
            raise HarnessInterrupted('Harness interrupted by signal ' + str(number))

    def defer(self):
        # Do not raise between OS spawn and publishing its Popen owner, or during teardown.
        self.deferred = True

    def resume(self):
        if self.received:
            raise HarnessInterrupted('Harness interrupted by signal ' + str(self.received[0]))
        self.deferred = False


CLIENT_EXIT_SECONDS = 180
STOP_EXIT_SECONDS = 60
GROUP_EXIT_SECONDS = 30
GROUP_SWEEPS = 8
_P_PID, _WNOHANG, _WEXITED, _WNOWAIT = 1, 0x1, 0x4, 0x20
_PROC_PGRP_ONLY, _PROC_PIDTBSDINFO = 2, 3
_libc = ctypes.CDLL(ctypes.util.find_library('c'), use_errno=True)
_libproc = ctypes.CDLL('/usr/lib/libproc.dylib', use_errno=True)


class _Siginfo(ctypes.Structure):
    _fields_ = [('signo', ctypes.c_int), ('errno', ctypes.c_int), ('code', ctypes.c_int), ('pid', ctypes.c_int),
                ('uid', ctypes.c_uint), ('status', ctypes.c_int), ('rest', ctypes.c_byte * 80)]


class _BsdInfo(ctypes.Structure):
    _fields_ = [('flags', ctypes.c_uint32), ('status', ctypes.c_uint32), ('xstatus', ctypes.c_uint32), ('pid', ctypes.c_uint32),
                ('ppid', ctypes.c_uint32), ('ids', ctypes.c_uint32 * 6), ('rfu', ctypes.c_uint32), ('comm', ctypes.c_char * 16),
                ('name', ctypes.c_char * 32), ('nfiles', ctypes.c_uint32), ('pgid', ctypes.c_uint32), ('pjobc', ctypes.c_uint32),
                ('tdev', ctypes.c_uint32), ('tpgid', ctypes.c_uint32), ('nice', ctypes.c_int32), ('start_sec', ctypes.c_uint64),
                ('start_usec', ctypes.c_uint64)]


def process_identity(pid):
    """PID plus start time names one process; None once it is gone or a zombie that can no longer run."""
    info = _BsdInfo()
    if _libproc.proc_pidinfo(pid, _PROC_PIDTBSDINFO, 0, ctypes.byref(info), ctypes.sizeof(info)) != ctypes.sizeof(info):
        return None
    return {'pid': pid, 'pgid': info.pgid, 'name': info.comm.decode(errors='replace'), 'started': [info.start_sec, info.start_usec]}


def describe(identity):
    return identity['name'] + ' pid ' + str(identity['pid']) + ' started ' + str(identity['started'][0]) + '.' + str(identity['started'][1]).zfill(6)


def live_group_members(pgid):
    pids = (ctypes.c_int * 4096)()
    size = _libproc.proc_listpids(_PROC_PGRP_ONLY, pgid, pids, ctypes.sizeof(pids))
    if size < 0 or size >= ctypes.sizeof(pids):
        raise OSError(ctypes.get_errno(), 'Process group ' + str(pgid) + ' could not be listed')
    members = [process_identity(pid) for pid in pids[:size // ctypes.sizeof(ctypes.c_int)] if pid]
    return [member for member in members if member and member['pgid'] == pgid]


def _exited_unreaped(pid, block=False):
    # WNOWAIT leaves the exited leader a zombie: its PID, and so its group number, cannot be reused (POSIX.1-2024 §4.17).
    info = _Siginfo()
    while _libc.waitid(_P_PID, pid, ctypes.byref(info), _WEXITED | _WNOWAIT | (0 if block else _WNOHANG)) != 0:
        if ctypes.get_errno() != errno.EINTR:
            raise OSError(ctypes.get_errno(), 'waitid ' + str(pid))
    return info.pid == pid


def _await_exits(processes, seconds):
    """Waits for NOTE_EXIT of each {pid: start time or None}; returns the PIDs still running at the deadline."""
    pending = set(processes)
    deadline = time.monotonic() + seconds
    queue = select.kqueue()
    try:
        events = queue.control([select.kevent(pid, select.KQ_FILTER_PROC, select.KQ_EV_ADD | select.KQ_EV_ONESHOT, select.KQ_NOTE_EXIT) for pid in pending], len(pending), 0)
        for pid, started in processes.items():
            if started is not None and (process_identity(pid) or {}).get('started') != started:
                pending.discard(pid)
        while True:
            for event in events:
                if event.flags & select.KQ_EV_ERROR and event.data != errno.ESRCH:
                    raise OSError(event.data, 'Exit of ' + str(event.ident) + ' cannot be observed')
                pending.discard(event.ident)
            remaining = deadline - time.monotonic()
            if not pending or remaining <= 0:
                return pending
            events = queue.control(None, len(pending), remaining)
    finally:
        queue.close()


def _signal_group(child):
    if child.returncode is not None:
        raise RuntimeError('Refusing to signal process group ' + str(child.pid) + ' after its leader was reaped')
    try:
        os.killpg(child.pid, signal.SIGKILL)
    except (ProcessLookupError, PermissionError):
        pass  # macOS answers EPERM when only the zombie leader is left; the listing below is the evidence.


def _sweep_group(child, observed):
    for _ in range(GROUP_SWEEPS):
        live = live_group_members(child.pid)
        if not live:
            return
        observed['group_survivors'].extend(member for member in live if member not in observed['group_survivors'])
        _signal_group(child)
        if _await_exits({member['pid']: member['started'] for member in live}, GROUP_EXIT_SECONDS):
            break
    raise RuntimeError('Process group ' + str(child.pid) + ' still has live members: ' + ', '.join(describe(member) for member in live_group_members(child.pid)))


def stop_leader(child):
    # Popen.terminate polls first, and polling would reap the leader before its group is swept.
    os.kill(child.pid, signal.SIGTERM)


def _settle(child, stopping, observed):
    """The leader is reaped only after its group holds nothing live, so no group signal can reach a reused number."""
    errors = observed['cleanup_errors']
    if stopping:
        try:
            if not _exited_unreaped(child.pid):
                stop_leader(child)
        except BaseException as error:
            errors.append('Client stop: ' + str(error))
    try:
        if _await_exits({child.pid: None}, STOP_EXIT_SECONDS):
            errors.append('Client still running ' + str(STOP_EXIT_SECONDS) + ' s after its stop; killing its process group')
            _signal_group(child)
            if _await_exits({child.pid: None}, GROUP_EXIT_SECONDS):
                raise RuntimeError('Client survived SIGKILL to its process group')
        # NOTE_EXIT can precede waitability; this blocks only for an exit already under way.
        assert _exited_unreaped(child.pid, block=True), 'Client exit must be observed before its group is swept'
        _sweep_group(child, observed)
    except BaseException as error:
        errors.append('Client process group: ' + str(error))
        return None
    if observed['group_survivors'] and not stopping:
        errors.append('Owned client left live process group members: ' + ', '.join(describe(member) for member in observed['group_survivors']))
    try:
        code = child.wait()
        observed.update(client_exit_waited=True, client_exit_code=code)
        return code
    except BaseException as error:
        errors.append('Client actual wait: ' + str(error))
        return None


def _owned_client(command, config, stdout, stderr, interruptions, observed, after_started=None):
    child, primary, code = None, None, None
    observed.update(client_started=False, client_exit_waited=False, cleanup_errors=[], group_survivors=[])
    interruptions.defer()
    try:
        child = subprocess.Popen(command, cwd=config['work'], env=config['env'], stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr, close_fds=True, start_new_session=True)
        observed.update(client_started=True, client_pid=child.pid)
        interruptions.resume()
        if after_started:
            after_started(child)
        if _await_exits({child.pid: None}, CLIENT_EXIT_SECONDS):
            raise TimeoutError('Owned client still running after ' + str(CLIENT_EXIT_SECONDS) + ' s')
    except BaseException as error:
        primary = error
    finally:
        interruptions.defer()
        if child is not None:
            code = _settle(child, primary is not None, observed)
    if primary is not None:
        raise primary
    if observed['cleanup_errors']:
        raise RuntimeError('; '.join(observed['cleanup_errors']))
    interruptions.resume()
    return code


def native_exit_evidence(value):
    if not isinstance(value, dict) or value.get('cleanupKnown') is not True:
        return False
    starts, exits = value.get('actualSpawns'), value.get('exits')
    if not isinstance(starts, list) or not isinstance(exits, list):
        return False
    if not all(type(pid) is int and pid > 0 for pid in starts) or len(starts) != len(set(starts)):
        return False
    if len(exits) != len(starts):
        return False
    return all(isinstance(exit, dict) and exit.get('actualExit') is True and type(exit.get('pid')) is int and exit['pid'] > 0 for exit in exits) and sorted(exit['pid'] for exit in exits) == sorted(starts)


def invoke(runtime, config, mode, interruptions, observed):
    report = runtime / 'report.json'
    report.unlink(missing_ok=True)
    command = ['/usr/bin/sandbox-exec', '-f', str(runtime / 'sandbox.sb'), str(runtime / 'node'), str(runtime / 'client.cjs'), str(runtime / 'config.json'), mode]
    try:
        with (runtime / (mode + '.stdout')).open('wb') as stdout, (runtime / (mode + '.stderr')).open('wb') as stderr:
            code = _owned_client(command, config, stdout, stderr, interruptions, observed)
    finally:
        try:
            if report.exists():
                assert report.stat().st_size <= MAX_BODY, 'Bounded child report required'
                observed['report'] = json.loads(report.read_bytes())
        except BaseException as error:
            observed['report_error'] = str(error)
    if observed.get('report_error'):
        raise RuntimeError('Child report: ' + observed['report_error'])
    return code, observed.get('report')

def main(interruptions):
    assert sys.platform == 'darwin' and os.uname().machine == 'arm64'
    assert Path('/usr/bin/sandbox-exec').is_file(), 'No isolation fallback'
    node, archive, client, metadata = [Path(p).resolve(strict=True) for p in sys.argv[1:]]
    parent = Path(os.environ['TEST_TMPDIR']).resolve(strict=True)
    assert not str(parent).startswith(('/Users/', '/home/')), 'Fixture must be outside real user homes'
    root = Path(tempfile.mkdtemp(prefix='claude-native-', dir=parent))
    runtime = root / 'runtime'
    model, server_thread, forbidden = None, None, None
    query_started, native_exit_known = False, False
    primary = None
    client_processes = {}
    evidence = {'client_processes': client_processes, 'scope': 'Official SDK + unmodified native harness + synthetic loopback model; text only', 'root': str(root)}
    try:
        runtime.mkdir()
        interruptions.resume()
        for name in ['home', 'tmp', 'work']:
            (runtime / name).mkdir()
        (runtime / 'home/config').mkdir()
        evidence['artifacts'] = {'node_sha256': copy_verified(node, runtime / 'node', True), 'native_sha256': native_from_archive(archive, runtime / 'claude'), 'client_sha256': copy_verified(client, runtime / 'client.cjs'), 'bundle_metadata_sha256': checksum(metadata)}
        marker = uuid.uuid4().hex
        (runtime / 'work/allowed-canary').write_text(marker)
        denied = root / 'denied-canary'
        denied.write_text('owned synthetic sibling ' + marker)
        forbidden = socket.socket()
        forbidden.bind(('127.0.0.1', 0))
        forbidden.listen(1)
        model = Model(marker, 'NATIVE_LOOPBACK_OK_' + marker)
        server_thread = threading.Thread(target=model.serve_forever)
        server_thread.start()
        env = {'HOME': str(runtime / 'home'), 'CLAUDE_CONFIG_DIR': str(runtime / 'home/config'), 'TMPDIR': str(runtime / 'tmp'), 'PATH': '/usr/bin:/bin', 'LANG': 'C.UTF-8', 'TZ': 'UTC', 'ANTHROPIC_API_KEY': TOKEN, 'ANTHROPIC_BASE_URL': 'http://127.0.0.1:' + str(model.server_port), 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1', 'DISABLE_AUTOUPDATER': '1'}
        config = {'env': env, 'native': str(runtime / 'claude'), 'work': str(runtime / 'work'), 'baseUrl': env['ANTHROPIC_BASE_URL'], 'marker': marker, 'prompt': 'Reply to this fixture marker: ' + marker, 'response': model.response, 'allowedFile': str(runtime / 'work/allowed-canary'), 'deniedFile': str(denied), 'deniedPort': forbidden.getsockname()[1], 'report': str(runtime / 'report.json')}
        (runtime / 'config.json').write_text(json.dumps(config))
        profile = sandbox_profile(runtime, model.server_port)
        (runtime / 'sandbox.sb').write_text(profile)
        evidence['sandbox_sha256'] = hashlib.sha256(profile.encode()).hexdigest()
        code, isolation = invoke(runtime, config, 'isolation', interruptions, client_processes.setdefault('isolation', {}))
        evidence['isolation'] = isolation
        assert code == 0 and isolation and isolation['status'] == 'passed', 'OS file/network isolation preflight failed; native must not launch'
        assert isolation['actualSpawns'] == [] and isolation['cleanupKnown']
        query_started = True
        code, result = invoke(runtime, config, 'query', interruptions, client_processes.setdefault('query', {}))
        evidence['query'] = result
        native_exit_known = native_exit_evidence(result)
        assert code == 0 and result and result['status'] == 'passed', 'Actual SDK/native roundtrip failed'
        assert native_exit_known and len(result['actualSpawns']) == len(result['exits']) == 1
        assert result['exits'][0]['actualExit'] and result['exits'][0]['code'] == 0
        assert not model.errors, model.errors
        requests = [r for r in model.requests if r['path'] == '/v1/messages']
        assert len(requests) == 1
        evidence['model_requests'] = model.requests
        evidence['status'] = 'passed'
    except BaseException as error:
        primary = error
        evidence['status'] = 'failed'
        evidence['error'] = str(error)
    finally:
        interruptions.defer()
        cleanup_errors = []
        operations = []
        query_report = client_processes.get('query', {}).get('report')
        native_exit_known = native_exit_evidence(query_report)
        if query_report is not None:
            evidence['query'] = query_report
        client_exit_known = all(not value.get('client_started') or value.get('client_exit_waited') for value in client_processes.values())
        if not client_exit_known:
            cleanup_errors.append('Owned client exit remains unresolved')
        for value in client_processes.values():
            cleanup_errors.extend(value.get('cleanup_errors', []))
            if value.get('report_error'):
                cleanup_errors.append('Child report: ' + value['report_error'])
        if model:
            if server_thread and server_thread.is_alive():
                operations.append(('server shutdown', model.shutdown))
            operations.append(('server handlers', model.server_close))
        if server_thread and server_thread.ident is not None:
            operations.append(('server thread', server_thread.join))
        if forbidden:
            operations.append(('canary listener', forbidden.close))
        for label, operation in operations:
            try:
                operation()
            except BaseException as error:
                cleanup_errors.append(label + ': ' + str(error))
        if model:
            evidence['model_requests'] = model.requests
            if model.errors:
                cleanup_errors.append('Model fixture: ' + repr(model.errors))
        evidence['native_exit_known'] = native_exit_known
        evidence['client_exit_known'] = client_exit_known
        evidence['interrupted_signals'] = interruptions.received
        if interruptions.received and primary is None:
            primary = HarnessInterrupted('Harness interrupted by signal ' + str(interruptions.received[0]))
            evidence['status'] = 'failed'
            evidence['error'] = str(primary)
        evidence['scratch_removed'] = False
        try:
            destination = Path(os.environ['TEST_UNDECLARED_OUTPUTS_DIR'])
            destination.mkdir(exist_ok=True)
            for path in runtime.glob('*.stderr'):
                shutil.copyfile(path, destination / path.name)
            query_client_not_started = not client_processes.get('query', {}).get('client_started')
            if client_exit_known and (not query_started or query_client_not_started or native_exit_known):
                shutil.rmtree(root)
                evidence['scratch_removed'] = True
            else:
                evidence['retained_unresolved_scratch'] = str(root)
            evidence['cleanup_errors'] = cleanup_errors
            if cleanup_errors:
                evidence['status'] = 'failed'
            (destination / 'report.json').write_text(json.dumps(evidence, indent=2) + '\n')
            print(json.dumps({'status': evidence['status'], 'scratch_removed': evidence['scratch_removed'], 'report': str(destination / 'report.json')}))
        except BaseException as error:
            cleanup_errors.append('Evidence/scratch cleanup: ' + str(error))
        if cleanup_errors and primary is None:
            primary = RuntimeError('; '.join(cleanup_errors))
        elif cleanup_errors:
            print('Cleanup also failed: ' + '; '.join(cleanup_errors), file=sys.stderr)
    if primary:
        raise primary


if __name__ == '__main__':
    with Interruptions() as interruptions:
        main(interruptions)
