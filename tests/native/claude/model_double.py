"""A local stand-in for the Messages API that the native Claude of the e2e sandbox talks to.

Usage: model_double.py <log> [<request scripts>]
Prints the port it listens on as its only line of stdout, then answers every /v1/messages over SSE
until it is killed. It accepts only the synthetic fixture key and never a saved bearer, and appends
each request's last user text, its tools' names and its context to <log>, one JSON line per request.

A request that offers the orchestrator's create_task is an orchestrator batch: for each request the
user created in that batch with a script in <request scripts> (the sandbox's `fakes/scripts`), it
calls create_task and then launch for every scripted task, one tool call per reply, and ends the
turn once they are all made. Auto mode's safety check of an action (it stops at `</block>`) is
answered with an allow. A task's request whose prompt holds the `when` text of a call scripted in
`model-tools/*.json` next to <log> ({"when", "id", "name", "input"}) and that offers its tool gets
that tool call, once per prompt; the request after its result is logged with it. `{plan_file}` in
a scripted input becomes the plan file Claude Code's plan mode named in the request. Any other
request gets one text reply.
"""
import glob
import http.server
import json
from pathlib import Path
import os
import re
import sys
import threading

MAX_BODY = 1048576
TOKEN = 'sk-ant-public-loopback-fixture-only'
REPLY = 'Done: the notes are drafted.'
ORCHESTRATOR = 'mcp__alondra-orchestrator__'


def last_user_text(body):
    for message in reversed(body.get('messages', [])):
        if message.get('role') != 'user':
            continue
        content = message.get('content')
        if isinstance(content, str):
            return content
        return '\n'.join(part.get('text', '') for part in content if isinstance(part, dict) and part.get('type') == 'text')
    return ''


def context_text(body):
    """Everything Claude Code put in front of the model as context: the system prompt, every
    user message's text and the tools' descriptions, where the skills it loaded are listed."""
    system = body.get('system')
    parts = [system] if isinstance(system, str) else [block.get('text', '') for block in system or [] if isinstance(block, dict)]
    for message in body.get('messages', []):
        content = message.get('content')
        if message.get('role') == 'user':
            parts.append(content if isinstance(content, str) else '\n'.join(part.get('text', '') for part in content if isinstance(part, dict)))
    parts.extend(tool.get('description', '') for tool in body.get('tools', []))
    return '\n'.join(part for part in parts if part)


def batch_text(body):
    """The text of the last user message that is a prompt rather than tool results."""
    for message in reversed(body.get('messages', [])):
        if message.get('role') != 'user':
            continue
        content = message.get('content')
        if isinstance(content, str):
            return content
        text = '\n'.join(part.get('text', '') for part in content if isinstance(part, dict) and part.get('type') == 'text')
        if not any(isinstance(part, dict) and part.get('type') == 'tool_result' for part in content):
            return text
    return ''


def made_since_prompt(body):
    """The tool calls already made in this turn, by id."""
    made = set()
    for message in reversed(body.get('messages', [])):
        content = message.get('content')
        if message.get('role') == 'user' and (isinstance(content, str) or not any(isinstance(part, dict) and part.get('type') == 'tool_result' for part in content)):
            break
        if message.get('role') == 'assistant' and isinstance(content, list):
            made.update(part.get('id') for part in content if isinstance(part, dict) and part.get('type') == 'tool_use')
    return made


def tool_results(body):
    """The tool results the conversation carries, with their text."""
    parts = [part for message in body.get('messages', []) if message.get('role') == 'user' and isinstance(message.get('content'), list) for part in message['content']]
    results = []
    for part in parts:
        if not isinstance(part, dict) or part.get('type') != 'tool_result':
            continue
        inner = part.get('content')
        text = inner if isinstance(inner, str) else '\n'.join(item.get('text', '') for item in inner or [] if isinstance(item, dict))
        results.append({'tool_use_id': part.get('tool_use_id'), 'is_error': part.get('is_error', False), 'text': text})
    return results


PLAN_FILE = re.compile(r'(?:plan file \(|plan at |exists at )(/[^\s()]+?\.md)')


def with_plan_file(value, body):
    """`value` with each `{plan_file}` replaced by the plan file Claude Code's plan mode names in
    the request; unchanged while it names none."""
    found = PLAN_FILE.search(json.dumps(body))
    if found is None:
        return value
    return json.loads(json.dumps(value).replace('{plan_file}', found.group(1)))


def task_calls(log, body, text):
    """The scripted tool calls whose `when` the prompt holds and whose tool the request offers."""
    offered = {tool.get('name') for tool in body.get('tools', [])}
    calls = []
    for path in sorted(Path(log).with_name('model-tools').glob('*.json')):
        call = json.loads(path.read_text())
        if call['when'] in text and call['name'] in offered:
            calls.append((call['id'], call['name'], with_plan_file(call['input'], body)))
    return calls


def orchestrator_calls(scripts, text):
    calls = []
    for path in sorted(glob.glob(os.path.join(scripts, '*.json'))):
        with open(path) as stream:
            script = json.load(stream)
        request = script['request_id']
        if not re.search(r'The user created Request ' + re.escape(request) + r'\b', text):
            continue
        for task in script['tasks']:
            launch = {'name': 'sandbox/' + task['task_id'], 'cwd': os.path.join(os.path.dirname(os.path.dirname(scripts)), 'repo'), 'model': 'sonnet', 'reason': 'A scripted sandbox node'}
            calls.append(('toolu_create_' + task['task_id'], 'create_task', {
                'task_id': task['task_id'], 'request_id': request, 'title': task['title'], 'agent_id': None,
                'review_prs': False, 'dependencies': [], 'launch': launch}))
            calls.append(('toolu_launch_' + task['task_id'], 'launch', {'task': task['task_id'], 'title': task['title'], 'model': 'sonnet', 'reason': 'A scripted sandbox node'}))
    return calls


# A task's requests stay unanswered while a file named `model-hold` is next to the log.
# A task's requests get this HTTP error while a file named `model-refuse` next to the log holds
# its status: 401 is a sign-in the API no longer takes, 529 an overloaded API.
ERRORS = {
    401: {'type': 'authentication_error', 'message': 'invalid x-api-key'},
    529: {'type': 'overloaded_error', 'message': 'Overloaded'},
}


def refusal(log):
    try:
        status = int(Path(log).with_name('model-refuse').read_text().strip())
    except (FileNotFoundError, ValueError):
        return None
    return status if status in ERRORS else None


class Model(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, log, scripts):
        super().__init__(('127.0.0.1', 0), Handler)
        self.log = log
        self.scripts = scripts
        self.guard = threading.Lock()
        self.served = 0


class Handler(http.server.BaseHTTPRequestHandler):
    protocol_version = 'HTTP/1.0'

    def log_message(self, *_):
        pass

    def reply(self, code, value, content_type='application/json'):
        raw = value if isinstance(value, bytes) else json.dumps(value).encode()
        self.send_response(code)
        self.send_header('content-type', content_type)
        self.send_header('content-length', str(len(raw)))
        self.send_header('request-id', 'req_sandbox_' + str(self.server.served))
        self.send_header('connection', 'close')
        self.end_headers()
        self.wfile.write(raw)
        self.wfile.flush()

    def refuse(self, why):
        self.reply(400, {'type': 'error', 'error': {'type': 'invalid_request_error', 'message': why}})

    def do_POST(self):
        length = int(self.headers.get('content-length', '0'))
        if not 0 < length <= MAX_BODY:
            return self.refuse('bounded Messages request required')
        body = json.loads(self.rfile.read(length))
        if self.headers.get('x-api-key') != TOKEN or self.headers.get('authorization') is not None:
            return self.refuse('only the synthetic fixture key is accepted')
        path = self.path.split('?', 1)[0]
        if path == '/v1/messages/count_tokens':
            return self.reply(200, {'input_tokens': 10})
        if path != '/v1/messages':
            return self.refuse('unexpected route ' + path)
        orchestrator = any(tool.get('name') == ORCHESTRATOR + 'create_task' for tool in body.get('tools', []))
        refused = refusal(self.server.log)
        if refused and not orchestrator:
            with self.server.guard, open(self.server.log, 'a') as stream:
                stream.write(json.dumps({'refused': refused, 'user': last_user_text(body)}) + '\n')
            return self.reply(refused, {'type': 'error', 'error': ERRORS[refused]})
        with self.server.guard:
            self.server.served += 1
            served = self.server.served
            with open(self.server.log, 'a') as stream:
                stream.write(json.dumps({'request': served, 'model': body.get('model'), 'user': last_user_text(body), 'messages': len(body.get('messages', [])), 'orchestrator': orchestrator,
                                         'tools': [tool.get('name') for tool in body.get('tools', [])], 'results': tool_results(body), 'context': context_text(body)}) + '\n')
        if not orchestrator and Path(self.server.log).with_name('model-hold').exists():
            # Held unanswered, as when the Mac shuts down before the model replies.
            threading.Event().wait()
        message = {'id': 'msg_sandbox_' + str(served), 'type': 'message', 'role': 'assistant', 'model': body.get('model'),
                   'content': [], 'stop_reason': None, 'stop_sequence': None, 'usage': {'input_tokens': 10, 'output_tokens': 0}}
        made = made_since_prompt(body)
        if orchestrator:
            calls = [(use, ORCHESTRATOR + tool, arguments) for use, tool, arguments in orchestrator_calls(self.server.scripts, batch_text(body))] if self.server.scripts else []
        else:
            calls = task_calls(self.server.log, body, batch_text(body))
        pending = [call for call in calls if call[0] not in made]
        stop_sequence = None
        if '</block>' in (body.get('stop_sequences') or []):
            block = {'type': 'text', 'text': ''}
            delta = {'type': 'text_delta', 'text': '<block>no'}
            stop, stop_sequence = 'stop_sequence', '</block>'
        elif pending:
            use, tool, arguments = pending[0]
            block = {'type': 'tool_use', 'id': use, 'name': tool, 'input': {}}
            delta = {'type': 'input_json_delta', 'partial_json': json.dumps(arguments)}
            stop = 'tool_use'
        else:
            block = {'type': 'text', 'text': ''}
            delta = {'type': 'text_delta', 'text': 'Batch handled.' if orchestrator else REPLY}
            stop = 'end_turn'
        events = [
            ('message_start', {'type': 'message_start', 'message': message}),
            ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': block}),
            ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': delta}),
            ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
            ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': stop, 'stop_sequence': stop_sequence}, 'usage': {'output_tokens': 10}}),
            ('message_stop', {'type': 'message_stop'}),
        ]
        if body.get('stream') is not True:
            text = block if block['type'] == 'tool_use' else dict(block, text=delta['text'])
            if text['type'] == 'tool_use':
                text = dict(block, input=json.loads(delta['partial_json']))
            return self.reply(200, dict(message, content=[text], stop_reason=stop, stop_sequence=stop_sequence,
                                        usage={'input_tokens': 10, 'output_tokens': 10}))
        data = ''.join('event: ' + name + '\ndata: ' + json.dumps(event) + '\n\n' for name, event in events).encode()
        self.reply(200, data, 'text/event-stream')


def main():
    model = Model(sys.argv[1], sys.argv[2] if len(sys.argv) > 2 else None)
    sys.stdout.write(str(model.server_address[1]) + '\n')
    sys.stdout.flush()
    sys.stdout.close()
    model.serve_forever()


if __name__ == '__main__':
    main()
