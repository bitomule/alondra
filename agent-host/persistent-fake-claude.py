#!/usr/bin/python3
import hashlib
import json
import os
import sys

capture = os.environ['FIXTURE_CAPTURE']

def flag(name):
    arguments = sys.argv[1:]
    for at, argument in enumerate(arguments):
        if argument.startswith(name + '='):
            return argument.split('=', 1)[1]
        if argument == name and at + 1 < len(arguments):
            return arguments[at + 1]
    return None

resumed = flag('--resume')
session_id = resumed or flag('--session-id')
# Like the native Claude, a session keeps its transcript on disk: a resumed one starts with it.
transcript = capture + '.transcript-' + session_id
history = sum(1 for _ in open(transcript)) if os.path.exists(transcript) else 0
if resumed and not history:
    raise RuntimeError('No conversation found with session ID: ' + resumed)
with open(capture + '.starts', 'a') as stream:
    stream.write(json.dumps(dict(pid=os.getpid(), session=session_id, resume=resumed, history=history)) + '\n')
    stream.flush()
    os.fsync(stream.fileno())
pending = []
permission = None
# Message uuids are unique across processes, as the native ones are: a resumed session never repeats them.
serial = (os.getpid() % 100000) * 1000000
# The API refuses every prompt, with a 401 for an expired sign-in or a 529 when it is overloaded,
# until the test marks it recovered with a file next to the capture.
refused = None

def send(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + '\n')
    sys.stdout.flush()

def lifecycle(message, state):
    global serial
    serial += 1
    send(dict(type='command_lifecycle', command_uuid=message['uuid'], state=state, session_id=session_id,
              uuid='00000000-0000-4000-8000-' + str(serial).zfill(12)))

REFUSALS = {401: ('authentication_failed', 'Failed to authenticate. API Error: 401 OAuth token has expired'),
            529: ('server_error', 'API Error: 529 Overloaded')}

def assistant(message, error=None, text='Fixture answer'):
    global serial
    serial += 1
    value = dict(type='assistant', session_id=session_id,
                 uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                 parent_tool_use_id=None,
                 message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant',
                              model='fixture', content=[dict(type='text', text=text)],
                              stop_reason=None, stop_sequence=None,
                              usage=dict(input_tokens=1, output_tokens=1, cache_creation_input_tokens=0, cache_read_input_tokens=0)),
                 user_message_uuid=message['uuid'],
                 user_message_uuids=[entry['uuid'] for entry in pending])
    if error:
        value['error'] = error
    send(value)

def refuse(message, status):
    global serial
    error, text = REFUSALS[status]
    assistant(message, error=error, text=text)
    serial += 1
    send(dict(type='result', subtype='success', is_error=True, api_error_status=status,
              session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
              duration_ms=0, duration_api_ms=0, num_turns=1, total_cost_usd=0, usage={}, modelUsage={},
              permission_denials=[], result=text,
              terminal_reason='api_error', stop_reason=None,
              user_message_uuid=message['uuid'], user_message_uuids=[entry['uuid'] for entry in pending]))
    for entry in pending:
        lifecycle(entry, 'completed')
    pending.clear()

def result(message, failed=False):
    global serial
    serial += 1
    value = dict(type='result', subtype='error_during_execution' if failed else 'success', is_error=failed,
                 session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                 duration_ms=0, duration_api_ms=0, num_turns=0 if failed else 1,
                 total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
                 user_message_uuid=message['uuid'])
    if failed:
        value['errors'] = ['Fixture delivery failed before consumption']
    else:
        value.update(result='Fixture result', terminal_reason='completed', user_message_uuids=[entry['uuid'] for entry in pending])
    send(value)
    for entry in pending:
        lifecycle(entry, 'completed')
    pending.clear()

def consume(message):
    global serial
    serial += 1
    send(dict(type='assistant', session_id=session_id,
              uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
              parent_tool_use_id=None,
              message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant',
                           model='fixture', content=[dict(type='text', text='Fixture consumed input')],
                           stop_reason=None, stop_sequence=None,
                           usage=dict(input_tokens=1, output_tokens=1, cache_creation_input_tokens=0, cache_read_input_tokens=0)),
              user_message_uuid=message['uuid'],
              user_message_uuids=[entry['uuid'] for entry in pending]))

for raw in sys.stdin:
    message = json.loads(raw)
    if message['type'] == 'control_request':
        subtype = message['request']['subtype']
        if subtype == 'initialize':
            descriptors = {}
            for fd in (0, 3, 4, 5):
                try:
                    stat = os.fstat(fd)
                    descriptors[str(fd)] = dict(dev=stat.st_dev, ino=stat.st_ino, mode=stat.st_mode)
                except OSError:
                    descriptors[str(fd)] = None
            with open(capture, 'w') as stream:
                json.dump(dict(args=sys.argv[1:], descriptors=descriptors, env_keys=sorted(os.environ), env_value_sha256={key: hashlib.sha256(value.encode()).hexdigest() for key, value in os.environ.items()}), stream)
            send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=dict(commands=[], models=[], account={}, output_style='default'))))
        elif subtype == 'interrupt':
            send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=dict(still_queued=[]))))
            if pending:
                result(pending[-1], failed=True)
        else:
            raise RuntimeError('Unexpected fixture control request')
    elif message['type'] == 'user':
        pending.append(message)
        # Like the native Claude 2.1.300: each prompt is queued and started, then init comes again.
        for state in ('queued', 'started'):
            lifecycle(message, state)
        if len(pending) == 1:
            send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-000000000100', claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='default', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
        with open(capture + '.prompts', 'a') as stream:
            stream.write(json.dumps(message['message']['content']) + '\n')
        with open(transcript, 'a') as stream:
            stream.write(json.dumps(message['message']['content']) + '\n')
            stream.flush()
            os.fsync(stream.fileno())
        text = message['message']['content']
        if text.endswith('signed-out'):
            refused = 401
        elif text.endswith('overloaded'):
            refused = 529
        if refused and os.path.exists(capture + '.recovered'):
            refused = None
            assistant(message)
            result(message)
        elif refused:
            refuse(message, refused)
        elif text == 'stop-cleanup':
            send(dict(type='system', subtype='status', status='compacting', uuid='00000000-0000-4000-8000-000000000103', session_id=session_id))
        elif text.endswith('unread') and not resumed:
            # The session ends before Claude shows it read the prompt: no turn, no answer.
            pass
        elif text == 'native-failure-now':
            sys.exit(7)
        elif text == 'failed-delivery':
            result(message, failed=True)
        elif text == 'consume-and-hold':
            consume(message)
        elif text == 'permission':
            permission = message
            send(dict(type='control_request', request_id='fixture-permission', request=dict(subtype='can_use_tool', tool_name='Bash', input=dict(command='fixture'), tool_use_id='fixture-tool')))
        elif text == 'background':
            # Like a native turn that starts an agent of its own: it takes the input, then the work starts.
            consume(message)
            send(dict(type='system', subtype='task_started', task_id='fixture-background', description='Fixture owned work', task_type='local_agent', uuid='00000000-0000-4000-8000-000000000101', session_id=session_id))
        else:
            if len(pending) > 1:
                send(dict(type='system', subtype='task_notification', task_id='fixture-background', status='completed', output_file='fixture-output', summary='Fixture work completed', uuid='00000000-0000-4000-8000-000000000102', session_id=session_id))
            result(message)
    elif message['type'] == 'control_response':
        with open(capture + '.decision', 'w') as stream:
            json.dump(message, stream)
        if permission:
            result(permission)
            permission = None
    elif message['type'] == 'control_cancel_request':
        pass
    else:
        raise RuntimeError('Unexpected fixture message')
