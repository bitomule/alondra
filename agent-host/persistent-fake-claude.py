#!/usr/bin/python3
import hashlib
import json
import os
import sys

capture = os.environ['FIXTURE_CAPTURE']
with open(capture + '.starts', 'a') as stream:
    stream.write(json.dumps(dict(pid=os.getpid())) + '\n')
    stream.flush()
    os.fsync(stream.fileno())
session_id = next(argument.split('=', 1)[1] for argument in sys.argv[1:] if argument.startswith('--session-id='))
pending = []
permission = None
serial = 0

def send(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + '\n')
    sys.stdout.flush()

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
    pending.clear()

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
            send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-000000000100', claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='default', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
        elif subtype == 'interrupt':
            send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=dict(still_queued=[]))))
            if pending:
                result(pending[-1], failed=True)
        else:
            raise RuntimeError('Unexpected fixture control request')
    elif message['type'] == 'user':
        pending.append(message)
        text = message['message']['content']
        if text == 'stop-cleanup':
            send(dict(type='system', subtype='status', status='compacting', uuid='00000000-0000-4000-8000-000000000103', session_id=session_id))
        elif text == 'native-failure-now':
            sys.exit(7)
        elif text == 'failed-delivery':
            result(message, failed=True)
        elif text == 'consume-and-hold':
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
        elif text == 'permission':
            permission = message
            send(dict(type='control_request', request_id='fixture-permission', request=dict(subtype='can_use_tool', tool_name='Bash', input=dict(command='fixture'), tool_use_id='fixture-tool')))
        elif text == 'background':
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
