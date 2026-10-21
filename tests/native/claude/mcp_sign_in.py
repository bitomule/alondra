"""Opt-in: the unmodified native Claude signs in to an MCP server through the Agent SDK's control
channel, with Alondra's part played as Alondra plays it. Only the MCP server and its OAuth
authorization server are doubles, on loopback; there is no model.

The client (mcp-client.mjs) runs two SDK sessions of the pinned Claude Code, asks the first for the
sign-in (`mcp_authenticate`), follows the authorization page as a browser would without following
its redirect, and hands that redirect to Claude's own listener the way the daemon does
(crates/alondrad/src/mcp_sign_in.rs). It then measures what the Agent SDK MCP plan left open:
PKCE, where the credential is kept, whether another session sees it, the refresh ahead of expiry,
and what a credential the server no longer accepts leaves.

Before that, Alondra's own host checks the project as it checks one no node runs in
(agent-host/probe.ts): it lists the server as signed out, and nothing reaches the Messages API.
"""
import base64
import hashlib
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import threading
import urllib.parse
import uuid

HERE = Path(__file__).resolve().parent
EXPIRES_IN = 120


def harness():
    spec = importlib.util.spec_from_file_location('native_fixture', HERE / 'run.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class OAuthMcp(http.server.ThreadingHTTPServer):
    daemon_threads = False

    def __init__(self):
        super().__init__(('127.0.0.1', 0), Handler)
        self.base = 'http://127.0.0.1:' + str(self.server_port)
        self.guard = threading.Lock()
        self.clients, self.codes, self.tokens, self.refresh = {}, {}, {}, {}
        self.log, self.errors = [], []
        self.revoked = False

    def record(self, entry):
        with self.guard:
            if len(self.log) < 256:
                self.log.append(entry)


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, *_):
        pass

    def reply(self, code, value=None, headers=()):
        raw = b'' if value is None else json.dumps(value).encode()
        self.send_response(code)
        for name, header in headers:
            self.send_header(name, header)
        if value is not None:
            self.send_header('content-type', 'application/json')
        self.send_header('content-length', str(len(raw)))
        self.send_header('connection', 'close')
        self.end_headers()
        self.wfile.write(raw)

    def body(self):
        length = int(self.headers.get('content-length', '0'))
        assert 0 <= length <= 65536, 'Bounded request'
        return self.rfile.read(length)

    def do_GET(self):
        server = self.server
        url = urllib.parse.urlsplit(self.path)
        query = dict(urllib.parse.parse_qsl(url.query))
        server.record({'method': 'GET', 'path': url.path})
        if url.path in ('/.well-known/oauth-protected-resource', '/.well-known/oauth-protected-resource/mcp'):
            return self.reply(200, {'resource': server.base + '/mcp', 'authorization_servers': [server.base], 'bearer_methods_supported': ['header']})
        if url.path == '/.well-known/oauth-authorization-server':
            return self.reply(200, {
                'issuer': server.base, 'authorization_endpoint': server.base + '/authorize', 'token_endpoint': server.base + '/token',
                'registration_endpoint': server.base + '/register', 'response_types_supported': ['code'],
                'grant_types_supported': ['authorization_code', 'refresh_token'], 'code_challenge_methods_supported': ['S256'],
                'token_endpoint_auth_methods_supported': ['none'],
            })
        if url.path == '/authorize':
            client = server.clients.get(query.get('client_id'))
            redirect = query.get('redirect_uri', '')
            if not client or redirect not in client['redirect_uris'] or query.get('response_type') != 'code':
                server.errors.append('authorize refused: ' + json.dumps(query))
                return self.reply(400, {'error': 'invalid_request'})
            code = uuid.uuid4().hex
            with server.guard:
                server.codes[code] = {'client_id': query['client_id'], 'redirect_uri': redirect, 'challenge': query.get('code_challenge'), 'method': query.get('code_challenge_method')}
            server.record({'authorize': {key: query.get(key) is not None for key in ('state', 'code_challenge', 'resource', 'scope')}, 'method_s256': query.get('code_challenge_method') == 'S256', 'redirect_uri': redirect})
            separator = '&' if '?' in redirect else '?'
            location = redirect + separator + urllib.parse.urlencode({'code': code, 'state': query.get('state', '')})
            return self.reply(302, None, [('location', location)])
        if url.path == '/fixture/revoke':
            server.revoked = True
            return self.reply(200, {'revoked': True})
        if url.path == '/mcp':
            return self.reply(405, {'error': 'no event stream'})
        server.record({'unexpected': url.path})
        return self.reply(404, {'error': 'not_found'})

    def do_POST(self):
        server = self.server
        try:
            url = urllib.parse.urlsplit(self.path)
            raw = self.body()
            if url.path == '/register':
                metadata = json.loads(raw)
                client_id = 'fixture-client-' + uuid.uuid4().hex[:8]
                with server.guard:
                    server.clients[client_id] = {'redirect_uris': metadata.get('redirect_uris', [])}
                server.record({'register': {'redirect_uris': metadata.get('redirect_uris'), 'auth_method': metadata.get('token_endpoint_auth_method')}})
                return self.reply(201, {**metadata, 'client_id': client_id})
            if url.path == '/token':
                form = dict(urllib.parse.parse_qsl(raw.decode()))
                grant = form.get('grant_type')
                if grant == 'authorization_code':
                    issued = server.codes.pop(form.get('code'), None)
                    verifier = form.get('code_verifier', '')
                    challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode()).digest()).rstrip(b'=').decode()
                    pkce = issued is not None and issued['method'] == 'S256' and issued['challenge'] == challenge
                    server.record({'token': 'authorization_code', 'pkce_verified': pkce, 'redirect_matches': issued is not None and form.get('redirect_uri') == issued['redirect_uri']})
                    if not pkce or form.get('redirect_uri') != issued['redirect_uri']:
                        return self.reply(400, {'error': 'invalid_grant'})
                elif grant == 'refresh_token':
                    known = form.get('refresh_token') in server.refresh
                    server.record({'token': 'refresh_token', 'known': known, 'revoked': server.revoked})
                    if not known or server.revoked:
                        return self.reply(400, {'error': 'invalid_grant'})
                else:
                    return self.reply(400, {'error': 'unsupported_grant_type'})
                access, refresh = 'at-' + uuid.uuid4().hex, 'rt-' + uuid.uuid4().hex
                with server.guard:
                    server.tokens[access] = True
                    server.refresh[refresh] = True
                return self.reply(200, {'access_token': access, 'token_type': 'Bearer', 'expires_in': EXPIRES_IN, 'refresh_token': refresh})
            if url.path == '/mcp':
                bearer = (self.headers.get('authorization') or '').removeprefix('Bearer ')
                if bearer not in server.tokens or server.revoked:
                    server.record({'mcp': 'unauthorized'})
                    return self.reply(401, {'error': 'invalid_token'}, [('www-authenticate', 'Bearer resource_metadata="' + server.base + '/.well-known/oauth-protected-resource"')])
                message = json.loads(raw)
                method = message.get('method')
                server.record({'mcp': method})
                if 'id' not in message:
                    return self.reply(202)
                if method == 'initialize':
                    result = {'protocolVersion': message['params']['protocolVersion'], 'capabilities': {'tools': {}}, 'serverInfo': {'name': 'fixture', 'version': '1'}}
                elif method == 'tools/list':
                    result = {'tools': [{'name': 'echo', 'description': 'Echoes', 'inputSchema': {'type': 'object', 'properties': {}}}]}
                elif method == 'tools/call':
                    result = {'content': [{'type': 'text', 'text': 'echo'}]}
                else:
                    return self.reply(200, {'jsonrpc': '2.0', 'id': message['id'], 'error': {'code': -32601, 'message': 'Method not found'}})
                return self.reply(200, {'jsonrpc': '2.0', 'id': message['id'], 'result': result})
            server.record({'unexpected': url.path})
            return self.reply(404, {'error': 'not_found'})
        except Exception as error:
            server.errors.append(str(error))
            return self.reply(500, {'error': 'fixture'})


# A double of macOS's `security` for the generic-password items Claude Code keeps its credentials in,
# as files in the synthetic HOME: Claude's own keychain code runs, and no real keychain is touched.
# Exit 44 is the tool's "not found".
SECURITY = """#!{node}
const {{ createHash }} = require('node:crypto');
const fs = require('node:fs');
const path = require('node:path');
const store = {store};
const item = (account, service) => path.join(store, createHash('sha256').update(account + '\\0' + service).digest('hex'));
const flag = (args, name) => args[args.indexOf(name) + 1];
function run(args) {{
  const [command] = args;
  if (command === 'find-generic-password') {{
    const file = item(flag(args, '-a'), flag(args, '-s'));
    if (!fs.existsSync(file)) return 44;
    process.stdout.write(fs.readFileSync(file, 'utf8') + '\\n');
    return 0;
  }}
  if (command === 'add-generic-password') {{
    fs.mkdirSync(store, {{ recursive: true, mode: 0o700 }});
    fs.writeFileSync(item(flag(args, '-a'), flag(args, '-s')), Buffer.from(flag(args, '-X'), 'hex').toString('utf8'), {{ mode: 0o600 }});
    return 0;
  }}
  if (command === 'delete-generic-password') {{
    const file = item(flag(args, '-a'), flag(args, '-s'));
    if (!fs.existsSync(file)) return 44;
    fs.unlinkSync(file);
    return 0;
  }}
  return 1;
}}
const argv = process.argv.slice(2);
if (argv[0] === '-i') {{
  const lines = fs.readFileSync(0, 'utf8').split('\\n').filter(Boolean);
  process.exitCode = lines.map((line) => run(line.match(/"[^"]*"|\\S+/g).map((word) => word.replace(/^"|"$/g, '')))).find((code) => code !== 0) ?? 0;
}} else {{
  process.exitCode = run(argv);
}}
"""


def free_port():
    with socket.socket() as probe:
        probe.bind(('127.0.0.1', 0))
        return probe.getsockname()[1]


def profile(runtime, server_port, callback_port):
    """run.py's profile, with what a sign-in adds: the MCP server's port, and Claude's own listener
    on the callback port, which the client also reaches to hand it the browser's return."""
    quote = lambda value: json.dumps(str(value))
    return '\n'.join([
        '(version 1)', '(deny default)', '(deny mach-lookup)',
        '(allow process-fork)', '(allow signal (target same-sandbox))',
        '(allow process-info* (target self))', '(allow sysctl-read)',
        '(allow process-exec (literal ' + quote(runtime / 'node') + ') (literal ' + quote(runtime / 'claude') + ') (literal ' + quote(runtime / 'bin/security') + ') (literal "/bin/sh"))',
        '(allow file-read* (literal "/bin/sh"))',
        '(allow file-read-data (literal "/"))',
        '(allow file-read-metadata ' + ' '.join('(literal ' + quote(parent) + ')' for parent in runtime.parents) + ')',
        '(allow file-read* (subpath ' + quote(runtime) + ') (subpath "/System/Library") (subpath "/usr/lib") (subpath "/usr/share/icu") (subpath "/private/var/db/dyld") (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom"))',
        '(allow file-write* (subpath ' + quote(runtime / 'home') + ') (subpath ' + quote(runtime / 'tmp') + ') (subpath ' + quote(runtime / 'work') + ') (literal ' + quote(runtime / 'report.json') + ') (literal "/dev/null"))',
        '(allow file-write* ' + ' '.join('(literal ' + quote(runtime / name) + ')' for name in ['sign-in.stdout', 'sign-in.stderr']) + ')',
        '(allow network-outbound (remote ip "localhost:' + str(server_port) + '") (remote ip "localhost:' + str(callback_port) + '"))',
        '(allow network-bind (local ip "localhost:' + str(callback_port) + '"))',
        '(allow network-inbound (local ip "localhost:' + str(callback_port) + '"))',
    ])


def probe(runtime, env, server):
    """Alondra's host checks the project without a turn: the server is listed, signed out, and the
    model double behind ANTHROPIC_BASE_URL gets no request."""
    config = json.dumps({'executable': str(runtime / 'claude'), 'cwd': str(runtime / 'work'), 'env': env}).encode()
    command = ['/usr/bin/sandbox-exec', '-f', str(runtime / 'sandbox.sb'), str(runtime / 'node'), str(runtime / 'host.cjs'), '--mcp-probe']
    host_env = {'HOME': env['HOME'], 'PATH': '/usr/bin:/bin'}
    child = subprocess.Popen(command, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, env=host_env, start_new_session=True)
    try:
        output, errors = child.communicate(config, timeout=120)
    finally:
        try:
            os.killpg(child.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        child.wait()
    lines = output.decode().splitlines()
    assert lines, 'The check wrote nothing: ' + errors.decode()[-2000:]
    answer = json.loads(lines[0])
    servers = answer.get('servers')
    assert servers is not None, answer
    assert [server_['status'] for server_ in servers if server_['name'] == 'fixture'] == ['needs-auth'], servers
    asked = [entry for entry in server.log if 'messages' in json.dumps(entry)]
    assert not asked, 'The check asked the model: ' + json.dumps(asked)
    return {'servers': servers, 'model_requests': len(asked), 'exit': child.returncode}


def main(fixture, interruptions):
    assert sys.platform == 'darwin' and os.uname().machine == 'arm64'
    assert Path('/usr/bin/sandbox-exec').is_file(), 'No isolation fallback'
    node, archive, client, metadata, host = [Path(p).resolve(strict=True) for p in sys.argv[1:]]
    parent = Path(os.environ['TEST_TMPDIR']).resolve(strict=True)
    assert not str(parent).startswith(('/Users/', '/home/')), 'Fixture must be outside real user homes'
    root = Path(tempfile.mkdtemp(prefix='claude-mcp-', dir=parent))
    runtime = root / 'runtime'
    server, thread, primary = None, None, None
    observed = {}
    evidence = {'scope': 'Official SDK + unmodified native Claude; MCP server and OAuth server are loopback doubles; no model', 'root': str(root)}
    try:
        runtime.mkdir()
        interruptions.resume()
        for name in ['home', 'tmp', 'work', 'home/config', 'bin']:
            (runtime / name).mkdir()
        evidence['artifacts'] = {
            'node_sha256': fixture.copy_verified(node, runtime / 'node', True),
            'native_sha256': fixture.native_from_archive(archive, runtime / 'claude'),
            'client_sha256': fixture.copy_verified(client, runtime / 'client.cjs'),
            'host_sha256': fixture.copy_verified(host, runtime / 'host.cjs'),
            'bundle_metadata_sha256': fixture.checksum(metadata),
        }
        security = runtime / 'bin/security'
        security.write_text(SECURITY.format(node=runtime / 'node', store=json.dumps(str(runtime / 'home/keychain'))))
        security.chmod(0o700)
        server = OAuthMcp()
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        callback_port = free_port()
        (runtime / 'home/config/.claude.json').write_text(json.dumps({'mcpServers': {'fixture': {'type': 'http', 'url': server.base + '/mcp'}}}))
        env = {
            'HOME': str(runtime / 'home'), 'CLAUDE_CONFIG_DIR': str(runtime / 'home/config'), 'TMPDIR': str(runtime / 'tmp'),
            'CLAUDE_CODE_TMPDIR': str(runtime / 'tmp'), 'PATH': str(runtime / 'bin') + ':/usr/bin:/bin', 'LANG': 'C.UTF-8', 'TZ': 'UTC',
            'ANTHROPIC_API_KEY': fixture.TOKEN, 'ANTHROPIC_BASE_URL': server.base, 'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1',
            'DISABLE_AUTOUPDATER': '1', 'MCP_OAUTH_CALLBACK_PORT': str(callback_port),
        }
        config = {'env': env, 'native': str(runtime / 'claude'), 'work': str(runtime / 'work'), 'server': server.base,
                  'keychain': str(runtime / 'home/keychain'), 'report': str(runtime / 'report.json')}
        (runtime / 'config.json').write_text(json.dumps(config))
        (runtime / 'sandbox.sb').write_text(profile(runtime, server.server_port, callback_port))
        evidence['probe'] = probe(runtime, env, server)
        command = ['/usr/bin/sandbox-exec', '-f', str(runtime / 'sandbox.sb'), str(runtime / 'node'), str(runtime / 'client.cjs'), str(runtime / 'config.json')]
        with (runtime / 'sign-in.stdout').open('wb') as stdout, (runtime / 'sign-in.stderr').open('wb') as stderr:
            code = fixture._owned_client(command, config, stdout, stderr, interruptions, observed)
        report = json.loads((runtime / 'report.json').read_bytes())
        evidence['client'] = report
        evidence['server_log'] = server.log
        assert code == 0 and report['status'] == 'passed', 'The native sign-in failed'
        assert fixture.native_exit_evidence(report), 'Every native Claude exited'
        steps = {step['name']: step for step in report['steps']}
        tokens = [entry for entry in server.log if 'token' in entry]
        assert any(entry['token'] == 'authorization_code' and entry['pkce_verified'] and entry['redirect_matches'] for entry in tokens), tokens
        assert steps['mcp_authenticate']['redirectScheme'] == 'localhost', 'No redirect of Alondra\'s own: the browser comes back to Claude'
        assert steps['status after the listener took the return']['status'] == 'connected', 'The return handed to Claude\'s listener completes the sign-in'
        assert 'mcp_oauth_callback_url' not in steps, 'The return never needs the control that would keep it on disk'
        assert steps['credential store']['keys'] == ['fixture|<hash>'], 'Claude keeps the sign-in in its own keychain item'
        assert steps['another session reconnects at once']['after'] == 'connected', 'One sign-in serves every node'
        assert any(entry['token'] == 'refresh_token' and not entry['revoked'] for entry in tokens), 'A token close to expiry is refreshed'
        assert steps['after the server stops accepting the sign-in']['status'] == 'needs-auth', 'A sign-in the server refuses is needs-auth again'
        assert not server.errors, server.errors
        evidence['status'] = 'passed'
    except BaseException as error:
        primary = error
        evidence['status'] = 'failed'
        evidence['error'] = str(error)
    finally:
        interruptions.defer()
        if server:
            server.shutdown()
            server.server_close()
        if thread and thread.ident is not None:
            thread.join()
        if server:
            evidence['server_log'] = server.log
            evidence['server_errors'] = server.errors
        evidence['client_processes'] = observed
        destination = Path(os.environ['TEST_UNDECLARED_OUTPUTS_DIR'])
        destination.mkdir(exist_ok=True)
        for path in runtime.glob('*.std*'):
            shutil.copyfile(path, destination / path.name)
        exited = not observed.get('client_started') or observed.get('client_exit_waited')
        if exited and fixture.native_exit_evidence(evidence.get('client')):
            shutil.rmtree(root)
            evidence['scratch_removed'] = True
        else:
            evidence['retained_unresolved_scratch'] = str(root)
        (destination / 'report.json').write_text(json.dumps(evidence, indent=2) + '\n')
        print(json.dumps({'status': evidence['status'], 'report': str(destination / 'report.json')}))
    if primary:
        raise primary


if __name__ == '__main__':
    native = harness()
    with native.Interruptions() as interruptions:
        main(native, interruptions)
