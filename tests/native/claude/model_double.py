"""A local stand-in for the Messages API that the native Claude of the e2e sandbox talks to.

Usage: model_double.py <log>
Prints the port it listens on as its only line of stdout, then answers every /v1/messages with one
text reply over SSE until it is killed. It accepts only the synthetic fixture key and never a saved
bearer, and appends each request's last user text to <log>, one JSON line per request.
"""
import http.server
import json
import sys
import threading

MAX_BODY = 1048576
TOKEN = 'sk-ant-public-loopback-fixture-only'
REPLY = 'Done: the notes are drafted.'


def last_user_text(body):
    for message in reversed(body.get('messages', [])):
        if message.get('role') != 'user':
            continue
        content = message.get('content')
        if isinstance(content, str):
            return content
        return '\n'.join(part.get('text', '') for part in content if isinstance(part, dict) and part.get('type') == 'text')
    return ''


class Model(http.server.ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, log):
        super().__init__(('127.0.0.1', 0), Handler)
        self.log = log
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
        with self.server.guard:
            self.server.served += 1
            served = self.server.served
            with open(self.server.log, 'a') as stream:
                stream.write(json.dumps({'request': served, 'model': body.get('model'), 'user': last_user_text(body)}) + '\n')
        message = {'id': 'msg_sandbox_' + str(served), 'type': 'message', 'role': 'assistant', 'model': body.get('model'),
                   'content': [], 'stop_reason': None, 'stop_sequence': None, 'usage': {'input_tokens': 10, 'output_tokens': 0}}
        events = [
            ('message_start', {'type': 'message_start', 'message': message}),
            ('content_block_start', {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}}),
            ('content_block_delta', {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': REPLY}}),
            ('content_block_stop', {'type': 'content_block_stop', 'index': 0}),
            ('message_delta', {'type': 'message_delta', 'delta': {'stop_reason': 'end_turn', 'stop_sequence': None}, 'usage': {'output_tokens': 10}}),
            ('message_stop', {'type': 'message_stop'}),
        ]
        if body.get('stream') is not True:
            return self.refuse('a streamed request is expected')
        data = ''.join('event: ' + name + '\ndata: ' + json.dumps(event) + '\n\n' for name, event in events).encode()
        self.reply(200, data, 'text/event-stream')


def main():
    model = Model(sys.argv[1])
    sys.stdout.write(str(model.server_address[1]) + '\n')
    sys.stdout.flush()
    sys.stdout.close()
    model.serve_forever()


if __name__ == '__main__':
    main()
