#!/usr/bin/python3
import errno
import fcntl
import json
import os
import socket
import sys


def wait_completion(root):
    with open(os.path.join(root, 'completion'), 'rb') as completion:
        fcntl.flock(completion, fcntl.LOCK_EX)


if len(sys.argv) == 3 and sys.argv[1] == '--fixture-wait-completion':
    wait_completion(sys.argv[2])
    sys.exit(0)

root = os.environ['FIXTURE_STATE']
session_id = next(arg.split('=', 1)[1] for arg in sys.argv[1:] if arg.startswith('--session-id='))


def send(value):
    sys.stdout.write(json.dumps(value) + '\n')
    sys.stdout.flush()


def write_pipe(fd, data):
    try:
        os.write(fd, data)
        return False
    except OSError as error:
        if error.errno != errno.EPIPE:
            raise
        return True


def descendant(ready):
    os.close(0)
    completion = os.open(os.path.join(root, 'completion'), os.O_CREAT | os.O_EXCL | os.O_RDWR, 0o600)
    fcntl.flock(completion, fcntl.LOCK_EX)
    listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
    listener.bind(os.path.join(root, 'd'))
    os.chmod(os.path.join(root, 'd'), 0o600)
    listener.listen(1)
    with open(os.path.join(root, 'capture.json'), 'w') as capture:
        json.dump(dict(root_pid=os.getppid(), descendant_pid=os.getpid(), session_id=session_id, home=os.environ['HOME']), capture)
    os.write(ready, b'R')
    os.close(ready)
    while True:
        connection, _ = listener.accept()
        with connection:
            command = b''
            while not command.endswith(b'\n'):
                chunk = connection.recv(64)
                if not chunk or len(command) + len(chunk) > 64:
                    os._exit(80)
                command += chunk
            if command == b'ping\n':
                status = dict(type='system', subtype='status', status=None, session_id=session_id,
                              uuid='00000000-0000-4000-8000-000000000201')
                stdout_broken = write_pipe(1, (json.dumps(status) + '\n').encode())
                stderr_broken = write_pipe(2, b'Owned fixture descendant remains alive\n')
                response = dict(alive=True, descendant_pid=os.getpid(), stdout_broken=stdout_broken, stderr_broken=stderr_broken)
                connection.sendall((json.dumps(response) + '\n').encode())
            elif command == b'stop\n':
                connection.sendall(b'{"stopping":true}\n')
                # Completion lock is released only by this process exiting.
                os._exit(0)
            else:
                os._exit(81)


for raw in sys.stdin:
    message = json.loads(raw)
    if message['type'] == 'control_request' and message['request']['subtype'] == 'initialize':
        send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'],
                  response=dict(commands=[], models=[], account={}, output_style='default'))))
        send(dict(type='system', subtype='init', session_id=session_id,
                  uuid='00000000-0000-4000-8000-000000000200', claude_code_version='2.1.300-fixture',
                  model='fixture', cwd=root, tools=[], mcp_servers=[], permissionMode='default',
                  slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
    elif message['type'] == 'user':
        reader, writer = os.pipe()
        pid = os.fork()
        if pid == 0:
            os.close(reader)
            try:
                descendant(writer)
            finally:
                os._exit(82)
        os.close(writer)
        if os.read(reader, 1) != b'R':
            os._exit(83)
        os.close(reader)
        send(dict(type='result', subtype='success', is_error=False, session_id=session_id,
                  uuid='00000000-0000-4000-8000-000000000202', duration_ms=0, duration_api_ms=0,
                  num_turns=1, total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
                  result='Fixture native root is exiting; descendant remains live', terminal_reason='completed',
                  user_message_uuid=message['uuid'], user_message_uuids=[message['uuid']]))
        os._exit(0)
    else:
        raise RuntimeError('Unexpected fixture message')
