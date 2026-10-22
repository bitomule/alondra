"""Opt-in: two Claude accounts as Alondra makes them, two CLAUDE_CONFIG_DIR folders that link what the
user set up in ~/.claude, with the unmodified native Claude and a loopback model double.

It checks that (a) the linked skills, agents, commands, rules, instructions and settings are loaded,
(b) a session created with folder A is resumed with folder B under the same id, (c) the linked
settings.json, and every other link, is still a link after each turn, and (d) a session whose turn
the API refused with the usage limit on A goes on with B under the same id, as Alondra moves a
node whose account reached its limit.
"""
import http.server
import importlib.util
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import threading
import uuid

HERE = Path(__file__).resolve().parent
SHARED = ['skills', 'agents', 'commands', 'plugins', 'rules', 'CLAUDE.md', 'settings.json', 'projects']
SETTINGS_MODEL = 'claude-haiku-4-5-20251001'


def harness():
    spec = importlib.util.spec_from_file_location('native_fixture', HERE / 'run.py')
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class Model(http.server.ThreadingHTTPServer):
    daemon_threads = False

    def __init__(self, token, markers):
        super().__init__(('127.0.0.1', 0), Handler)
        self.token, self.markers = token, markers
        self.requests, self.errors = [], []
        self.guard = threading.Lock()


def sse(model, marker, text):
    message = {'id': 'msg_' + marker, 'type': 'message', 'role': 'assistant', 'model': model, 'content': [], 'stop_reason': None,
               'stop_sequence': None, 'usage': {'input_tokens': 10, 'output_tokens': 0}}
    events = [
        ('message_start', {'type': 'message_start', 'message': message}),
        ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}),
        ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': text}}),
        ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
        ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 10}}),
        ('message_stop', {'type': 'message_stop'}),
    ]
    return ''.join('event: ' + name + '\ndata: ' + json.dumps(event) + '\n\n' for name, event in events).encode()


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, *_):
        pass

    def reply(self, code, raw, content_type):
        self.send_response(code)
        self.send_header('content-type', content_type)
        self.send_header('content-length', str(len(raw)))
        self.send_header('connection', 'close')
        self.end_headers()
        self.wfile.write(raw)
        self.wfile.flush()

    def limited(self):
        """The API's answer once the account's usage limit is reached: a 429 it says not to retry."""
        raw = json.dumps({'type': 'error', 'error': {'type': 'rate_limit_error', 'message': 'This request would exceed your account\'s rate limit.'}}).encode()
        self.send_response(429)
        for name, value in [('content-type', 'application/json'), ('content-length', str(len(raw))), ('connection', 'close'),
                            ('retry-after', '0'), ('x-should-retry', 'false')]:
            self.send_header(name, value)
        self.end_headers()
        self.wfile.write(raw)
        self.wfile.flush()

    def do_GET(self):
        self.server.errors.append('Unexpected GET ' + self.path)
        self.reply(404, b'{}', 'application/json')

    def do_POST(self):
        server, markers = self.server, self.server.markers
        try:
            self.connection.settimeout(30)  # Diagnostics for an incomplete request, never a timing assertion.
            length = int(self.headers.get('content-length', '0'))
            assert 0 < length <= 4 * 1048576, 'Bounded Messages request required'
            body = json.loads(self.rfile.read(length))
            assert self.headers.get('x-api-key') == server.token, 'Only the synthetic fixture key'
            path = self.path.split('?', 1)[0]
            assert path in ['/v1/messages', '/v1/messages/count_tokens'], 'Unexpected fixture route ' + path
            if path.endswith('/count_tokens'):
                return self.reply(200, json.dumps({'input_tokens': 10}).encode(), 'application/json')
            text = json.dumps(body)
            users = [json.dumps(m.get('content')) for m in body.get('messages', []) if m.get('role') == 'user']
            last = users[-1] if users else ''
            if markers['after'] in last:
                turn = 'after'
            elif markers['limited'] in last:
                turn = 'limited'
            elif markers['second'] in last:
                turn = 'second'
            elif markers['first'] in last:
                turn = 'first'
            else:
                turn = 'side'
            entry = {'turn': turn, 'model': body.get('model'), 'stream': body.get('stream'),
                     'instructions': markers['instructions'] in text, 'rule': markers['rule'] in text,
                     'first_prompt_in_history': markers['first'] in text, 'first_answer_in_history': markers['first_answer'] in text,
                     'limited_prompt_in_history': markers['limited'] in text}
            with server.guard:
                assert len(server.requests) < 32, 'Bounded request journal'
                server.requests.append(entry)
            if body.get('stream') is not True:
                return self.reply(200, json.dumps({'id': 'msg_side', 'type': 'message', 'role': 'assistant', 'model': body.get('model'),
                    'content': [{'type': 'text', 'text': 'side'}], 'stop_reason': 'end_turn', 'stop_sequence': None,
                    'usage': {'input_tokens': 1, 'output_tokens': 1}}).encode(), 'application/json')
            if turn == 'limited':
                return self.limited()
            answer = {'first': markers['first_answer'], 'second': markers['second_answer'], 'after': markers['after_answer'], 'side': 'side'}[turn]
            self.reply(200, sse(body.get('model'), uuid.uuid4().hex, answer), 'text/event-stream')
        except Exception as error:
            server.errors.append(str(error))
            self.reply(409, json.dumps({'type': 'error', 'error': {'type': 'invalid_request_error', 'message': str(error)}}).encode(), 'application/json')


def profile(runtime, port):
    """run.py's profile, with this test's own output files."""
    quote = lambda value: json.dumps(str(value))
    return '\n'.join([
        '(version 1)', '(deny default)', '(deny mach-lookup)',
        '(allow process-fork)', '(allow signal (target same-sandbox))',
        '(allow process-info* (target self))', '(allow sysctl-read)',
        '(allow process-exec (literal ' + quote(runtime / 'node') + ') (literal ' + quote(runtime / 'claude') + '))',
        '(allow file-read-data (literal "/"))',
        '(allow file-read-metadata ' + ' '.join('(literal ' + quote(parent) + ')' for parent in runtime.parents) + ')',
        '(allow file-read* (subpath ' + quote(runtime) + ') (subpath "/System/Library") (subpath "/usr/lib") (subpath "/usr/share/icu") (subpath "/private/var/db/dyld") (literal "/dev/null") (literal "/dev/random") (literal "/dev/urandom"))',
        '(allow file-write* (subpath ' + quote(runtime / 'home') + ') (subpath ' + quote(runtime / 'tmp') + ') (subpath ' + quote(runtime / 'work') + ') (literal ' + quote(runtime / 'report.json') + ') (literal "/dev/null"))',
        '(allow file-write* (literal ' + quote(runtime / 'accounts.stdout') + ') (literal ' + quote(runtime / 'accounts.stderr') + '))',
        '(allow network-outbound (remote ip "localhost:' + str(port) + '"))',
    ])


def seed_shared(claude, markers):
    """What a user's ~/.claude holds, each with a marker the turn can show was read."""
    (claude / 'skills/accounts-fixture').mkdir(parents=True)
    (claude / 'skills/accounts-fixture/SKILL.md').write_text('---\nname: accounts-fixture\ndescription: Fixture skill ' + markers['skill'] + '\n---\nSay hello.\n')
    (claude / 'agents').mkdir()
    (claude / 'agents/accounts-agent.md').write_text('---\nname: accounts-agent\ndescription: Fixture agent\n---\nYou are a fixture.\n')
    (claude / 'commands').mkdir()
    (claude / 'commands/accounts-command.md').write_text('Fixture command.\n')
    (claude / 'rules').mkdir()
    (claude / 'rules/accounts-rule.md').write_text('Fixture rule ' + markers['rule'] + '\n')
    (claude / 'plugins').mkdir()
    (claude / 'projects').mkdir()
    (claude / 'CLAUDE.md').write_text('Fixture instructions ' + markers['instructions'] + '\n')
    (claude / 'settings.json').write_text(json.dumps({'model': SETTINGS_MODEL}) + '\n')


def account_folder(folder, claude):
    """As crates/core/src/claude_account.rs `prepare` makes it: private, each shared name a link."""
    folder.mkdir(mode=0o700, parents=True)
    for name in SHARED:
        os.symlink(claude / name, folder / name)


def main(fixture, interruptions):
    assert sys.platform == 'darwin' and os.uname().machine == 'arm64'
    assert Path('/usr/bin/sandbox-exec').is_file(), 'No isolation fallback'
    node, archive, client, metadata = [Path(p).resolve(strict=True) for p in sys.argv[1:]]
    parent = Path(os.environ['TEST_TMPDIR']).resolve(strict=True)
    assert not str(parent).startswith(('/Users/', '/home/')), 'Fixture must be outside real user homes'
    root = Path(tempfile.mkdtemp(prefix='claude-accounts-', dir=parent))
    runtime = root / 'runtime'
    model, thread, primary = None, None, None
    observed = {}
    evidence = {'scope': 'Official SDK + unmodified native Claude with two CLAUDE_CONFIG_DIR folders linked to one ~/.claude; loopback model double', 'root': str(root)}
    try:
        runtime.mkdir()
        interruptions.resume()
        for name in ['home', 'tmp', 'work']:
            (runtime / name).mkdir()
        evidence['artifacts'] = {
            'node_sha256': fixture.copy_verified(node, runtime / 'node', True),
            'native_sha256': fixture.native_from_archive(archive, runtime / 'claude'),
            'client_sha256': fixture.copy_verified(client, runtime / 'client.cjs'),
            'bundle_metadata_sha256': fixture.checksum(metadata),
        }
        markers = {key: key + '-' + uuid.uuid4().hex for key in ['first', 'second', 'first_answer', 'second_answer', 'limited', 'after', 'after_answer', 'instructions', 'rule', 'skill']}
        claude = runtime / 'home/.claude'
        claude.mkdir()
        seed_shared(claude, markers)
        accounts = {'a': runtime / 'home/.alondra/accounts/a', 'b': runtime / 'home/.alondra/accounts/b'}
        for folder in accounts.values():
            account_folder(folder, claude)
        settings_before = (claude / 'settings.json').read_text()
        model = Model(fixture.TOKEN, markers)
        thread = threading.Thread(target=model.serve_forever)
        thread.start()
        env = {'HOME': str(runtime / 'home'), 'TMPDIR': str(runtime / 'tmp'), 'CLAUDE_CODE_TMPDIR': str(runtime / 'tmp'), 'PATH': '/usr/bin:/bin',
               'LANG': 'C.UTF-8', 'TZ': 'UTC', 'ANTHROPIC_API_KEY': fixture.TOKEN, 'ANTHROPIC_BASE_URL': 'http://127.0.0.1:' + str(model.server_port),
               'CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC': '1', 'DISABLE_AUTOUPDATER': '1', 'CLAUDE_CODE_MAX_RETRIES': '0'}
        config = {'env': env, 'native': str(runtime / 'claude'), 'work': str(runtime / 'work'), 'report': str(runtime / 'report.json'),
                  'accounts': {name: str(folder) for name, folder in accounts.items()}, 'shared': SHARED,
                  'first': {'prompt': 'First turn, account A: ' + markers['first'], 'response': markers['first_answer']},
                  'second': {'prompt': 'Second turn, account B: ' + markers['second'], 'response': markers['second_answer']},
                  'limited': {'prompt': 'A turn the usage limit of account A refuses: ' + markers['limited']},
                  'after': {'prompt': 'The same session goes on with account B: ' + markers['after'], 'response': markers['after_answer']}}
        (runtime / 'config.json').write_text(json.dumps(config))
        (runtime / 'sandbox.sb').write_text(profile(runtime, model.server_port))
        command = ['/usr/bin/sandbox-exec', '-f', str(runtime / 'sandbox.sb'), str(runtime / 'node'), str(runtime / 'client.cjs'), str(runtime / 'config.json')]
        with (runtime / 'accounts.stdout').open('wb') as stdout, (runtime / 'accounts.stderr').open('wb') as stderr:
            code = fixture._owned_client(command, config, stdout, stderr, interruptions, observed)
        report = json.loads((runtime / 'report.json').read_bytes())
        evidence['client'] = report
        evidence['model_requests'] = model.requests
        assert code == 0 and report['status'] == 'passed', 'The two turns did not finish'
        assert fixture.native_exit_evidence(report), 'Every native Claude exited'
        first, second, limited, after = report['turns']
        checks = {}
        checks['a_skills_agents_commands_loaded'] = 'accounts-fixture' in (first['skills'] or []) and 'accounts-agent' in (first['agents'] or []) and 'accounts-command' in (first['slashCommands'] or [])
        checks['b_skills_agents_commands_loaded'] = 'accounts-fixture' in (second['skills'] or []) and 'accounts-agent' in (second['agents'] or []) and 'accounts-command' in (second['slashCommands'] or [])
        checks['settings_model_used'] = first['model'] == SETTINGS_MODEL == second['model']
        main_requests = {turn: [r for r in model.requests if r['turn'] == turn] for turn in ['first', 'second', 'limited', 'after']}
        checks['one_request_per_turn'] = all(len(main_requests[turn]) == 1 for turn in ['first', 'second', 'limited', 'after'])
        checks['instructions_and_rules_reached_the_model'] = all(r['instructions'] and r['rule'] and r['model'] == SETTINGS_MODEL for r in main_requests['first'] + main_requests['second'])
        checks['b_resumed_a_session_with_the_same_id'] = bool(first['session']) and second['session'] == first['session'] == second['resultSession']
        checks['b_saw_the_conversation_of_a'] = all(r['first_prompt_in_history'] and r['first_answer_in_history'] for r in main_requests['second'])
        checks['a_turn_refused_by_the_usage_limit'] = limited['isError'] is True and limited['error'] == 'rate_limit'
        checks['b_went_on_with_the_refused_session_under_the_same_id'] = bool(limited['session']) and after['session'] == limited['session'] == after['resultSession'] and after['resumed'] == limited['session']
        checks['b_saw_the_refused_prompt'] = all(r['limited_prompt_in_history'] for r in main_requests['after'])
        checks['b_answered_after_the_limit'] = after['result'] == config['after']['response'] and after['isError'] is False
        transcripts = sorted(str(p.relative_to(claude)) for p in (claude / 'projects').rglob('*.jsonl'))
        evidence['transcripts_in_shared_projects'] = transcripts
        checks['transcript_kept_in_the_shared_projects'] = all(any(p.endswith(session + '.jsonl') for p in transcripts) for session in [first['session'], limited['session']])
        linked = lambda folder: {name: {'link': str(claude / name)} for name in SHARED}
        checks['every_name_still_a_link'] = all(links == linked(None) for links in report['links'].values())
        checks['shared_settings_unchanged'] = (claude / 'settings.json').read_text() == settings_before
        evidence['own_files'] = {name: sorted(p.name for p in folder.iterdir() if p.name not in SHARED) for name, folder in accounts.items()}
        evidence['checks'] = checks
        assert all(checks.values()), {name: ok for name, ok in checks.items() if not ok}
        assert not model.errors, model.errors
        evidence['status'] = 'passed'
    except BaseException as error:
        primary = error
        evidence['status'] = 'failed'
        evidence['error'] = str(error)
    finally:
        interruptions.defer()
        if model:
            model.shutdown()
            model.server_close()
        if thread and thread.ident is not None:
            thread.join()
        if model:
            evidence['model_requests'] = model.requests
            evidence['model_errors'] = model.errors
        evidence['client_processes'] = observed
        destination = Path(os.environ['TEST_UNDECLARED_OUTPUTS_DIR'])
        destination.mkdir(exist_ok=True)
        for path in runtime.glob('*.std*'):
            shutil.copyfile(path, destination / path.name)
        exited = not observed.get('client_started') or observed.get('client_exit_waited')
        if exited and (not observed.get('client_started') or fixture.native_exit_evidence(evidence.get('client'))):
            shutil.rmtree(root)
            evidence['scratch_removed'] = True
        else:
            evidence['retained_unresolved_scratch'] = str(root)
        (destination / 'report.json').write_text(json.dumps(evidence, indent=2) + '\n')
        print(json.dumps({'status': evidence['status'], 'checks': evidence.get('checks'), 'report': str(destination / 'report.json')}, indent=2))
    if primary:
        raise primary


if __name__ == '__main__':
    native = harness()
    with native.Interruptions() as interruptions:
        main(native, interruptions)
