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
    stream.write(json.dumps(dict(pid=os.getpid(), session=session_id, resume=resumed, history=history, cwd=os.getcwd())) + '\n')
    stream.flush()
    os.fsync(stream.fileno())
pending = []
# Each decision Claude asked for and still waits on, by request id.
asked = {}
decisions = 0
# Message uuids are unique across processes, as the native ones are: a resumed session never repeats them.
serial = (os.getpid() % 100000) * 1000000
# The API refuses every prompt, with a 401 for an expired sign-in or a 529 when it is overloaded,
# until the test marks it recovered with a file next to the capture.
refused = None

def send(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + '\n')
    sys.stdout.flush()

def control(message, response):
    with open(capture + '.controls', 'a') as stream:
        stream.write(json.dumps(message['request']) + '\n')
        stream.flush()
        os.fsync(stream.fileno())
    send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=response)))

def control_error(message, error):
    with open(capture + '.controls', 'a') as stream:
        stream.write(json.dumps(message['request']) + '\n')
        stream.flush()
        os.fsync(stream.fileno())
    send(dict(type='control_response', response=dict(subtype='error', request_id=message['request_id'], error=error)))

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

def consume(message, block=None):
    global serial
    serial += 1
    send(dict(type='assistant', session_id=session_id,
              uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
              parent_tool_use_id=None,
              message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant',
                           model='fixture', content=[block or dict(type='text', text='Fixture consumed input')],
                           stop_reason=None, stop_sequence=None,
                           usage=dict(input_tokens=1, output_tokens=1, cache_creation_input_tokens=0, cache_read_input_tokens=0)),
              user_message_uuid=message['uuid'],
              user_message_uuids=[entry['uuid'] for entry in pending]))

# Like the native Claude 2.1.300 with a foreground Bash that runs for seconds: it tracks the command
# as its own work, not backgrounded, which ends inside the turn that waits for it.
foreground = None

def foreground_started(message):
    global serial, foreground
    consume(message, dict(type='tool_use', id='toolu_fixture_foreground', name='Bash', input=dict(command='sleep 4')))
    serial += 1
    foreground = 'fixture-foreground-' + str(serial)
    send(dict(type='system', subtype='task_started', task_id=foreground, tool_use_id='toolu_fixture_foreground', description='sleep 4',
              task_type='local_bash', is_backgrounded=False, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), session_id=session_id))

def foreground_ended(status):
    global serial, foreground
    serial += 1
    send(dict(type='system', subtype='task_notification', task_id=foreground, tool_use_id='toolu_fixture_foreground', status=status,
              output_file='fixture-output', summary='sleep 4', uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), session_id=session_id))
    foreground = None

ALONDRA_TOOL = 'mcp__plugin_alondra_alondra__ask'
# Claude Code names a plugin's MCP server `plugin:<plugin>:<server>`; its tools `mcp__plugin_<plugin>_<server>__<tool>`.
ALONDRA_SERVER = 'plugin:alondra:alondra'
FIXTURE_PLAN = '# Fixture plan\n\n- [ ] Change the fixture\n- [ ] Test the fixture'
QUESTION = dict(question='Which fixture color?', header='Color', multiSelect=False,
                options=[dict(label='Red', description='The red fixture'), dict(label='Blue', description='The blue fixture')])
SECOND_QUESTION = dict(question='Which fixture sizes?', header='Sizes', multiSelect=True,
                       options=[dict(label='Small', description='The small fixture'), dict(label='Large', description='The large fixture')])

def decide(message, scenario, argument):
    global decisions
    model_input = None
    if scenario == 'bash':
        tool, tool_input, extra = 'Bash', dict(command=argument or 'echo fixture'), {}
    elif scenario == 'huge-bash':
        tool, tool_input, extra = 'Bash', dict(command='echo ' + 'x' * (2 * 1024 * 1024)), {}
    elif scenario in ('question', 'questions'):
        tool, tool_input, extra = 'AskUserQuestion', dict(questions=[QUESTION, SECOND_QUESTION] if scenario == 'questions' else [QUESTION]), {}
    elif scenario in ('plan', 'unwritten-plan'):
        # Like the native Claude 2.1.300: the model calls ExitPlanMode with no plan, and Claude Code
        # asks with the plan file's content and path, or with nothing while no plan file exists.
        tool, extra, model_input, tool_input = 'ExitPlanMode', {}, {}, {}
        if scenario == 'plan':
            plan_file = capture + '.plan.md'
            with open(plan_file, 'w') as stream:
                stream.write(argument or FIXTURE_PLAN)
            tool_input = dict(plan=argument or FIXTURE_PLAN, planFilePath=plan_file)
    elif scenario in ('alondra', 'imitation'):
        tool, tool_input = ALONDRA_TOOL, dict(need='Fixture need')
        extra = dict(mcp_server=dict(name=ALONDRA_SERVER, source='plugin' if scenario == 'alondra' else 'user'))
    elif scenario == 'elicitation':
        tool = None
    else:
        raise RuntimeError('Unknown fixture decision')
    decisions += 1
    request_id = 'decision-' + str(decisions)
    tool_use_id = None if tool is None else 'toolu_fixture_' + str(decisions)
    consume(message, None if tool is None else dict(type='tool_use', id=tool_use_id, name=tool, input=tool_input if model_input is None else model_input))
    if tool is None:
        request = dict(subtype='elicitation', mcp_server_name='fixture-server', message='Which fixture name?', mode='form',
                       requested_schema=dict(type='object', properties=dict(name=dict(type='string', title='Name')), required=['name']))
    else:
        request = dict(subtype='can_use_tool', tool_name=tool, input=tool_input, tool_use_id=tool_use_id, **extra)
    asked[request_id] = dict(message=message, tool_name=tool, tool_use_id=tool_use_id, cancelled=False)
    send(dict(type='control_request', request_id=request_id, request=request))

def decided(message):
    global serial
    response = message['response']
    entry = asked.pop(response['request_id'], None)
    if entry is None:
        raise RuntimeError('Response to a request the fixture never sent')
    record = dict(request_id=response['request_id'], subtype='elicitation' if entry['tool_name'] is None else 'can_use_tool',
                  cancelled=entry['cancelled'], outcome=response['subtype'], response=response.get('response'))
    if entry['tool_name'] is not None:
        record.update(tool_name=entry['tool_name'], tool_use_id=entry['tool_use_id'])
    with open(capture + '.decisions', 'a') as stream:
        stream.write(json.dumps(record, ensure_ascii=False) + '\n')
        stream.flush()
        os.fsync(stream.fileno())
    if entry['cancelled']:
        return
    answer = response.get('response') or {}
    if entry['tool_name'] is not None:
        serial += 1
        allowed = answer.get('behavior') == 'allow'
        send(dict(type='user', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), parent_tool_use_id=None,
                  message=dict(role='user', content=[dict(type='tool_result', tool_use_id=entry['tool_use_id'], is_error=not allowed,
                                                          content=json.dumps(answer.get('updatedInput')) if allowed else answer.get('message', 'denied'))])))
    result(entry['message'])

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
            send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=dict(commands=[], models=[dict(value='fixture-model', displayName='Fixture model', description='The model this fixture offers')], account={}, output_style='default'))))
        elif subtype == 'interrupt':
            control(message, dict(still_queued=[]))
            # The native Claude withdraws what it still waits on; the SDK aborts that callback and answers it.
            for request_id, entry in asked.items():
                if not entry['cancelled']:
                    entry['cancelled'] = True
                    send(dict(type='control_cancel_request', request_id=request_id))
            if foreground:
                foreground_ended('stopped')
            if pending:
                result(pending[-1], failed=True)
        elif subtype in ('set_model', 'set_permission_mode'):
            control(message, {})
        elif subtype == 'mcp_status':
            control(message, dict(mcpServers=[dict(name='fixture', status='connected')]))
        elif subtype == 'mcp_reconnect':
            if message['request'].get('serverName') == 'fixture':
                control(message, {})
            else:
                control_error(message, 'Server not found: ' + str(message['request'].get('serverName')))
        elif subtype == 'mcp_authenticate':
            server = message['request'].get('serverName')
            if server == 'fixture':
                control(message, dict(requiresUserAction=False, callbackExpected=False))
            elif server == 'linear':
                control(message, dict(authUrl='https://auth.example.com/authorize?client_id=fixture&state=fixture-state', requiresUserAction=True, callbackExpected=True, redirectScheme='localhost', state='fixture-state', callbackPort=49200))
            elif server == 'claude.ai Gmail':
                control(message, dict(authUrl='https://claude.ai/api/organizations/o/mcp/start-auth/g', requiresUserAction=True, callbackExpected=False))
            elif server == 'plain':
                control(message, dict(authUrl='http://auth.example.com/authorize', requiresUserAction=True, callbackExpected=True, redirectScheme='localhost', state='s', callbackPort=49201))
            else:
                control_error(message, 'Server not found: ' + str(server))
        else:
            raise RuntimeError('Unexpected fixture control request')
    elif message['type'] == 'user':
        pending.append(message)
        # The prompt is captured before Claude says anything about it: whoever reads one of its
        # events knows the capture holds it.
        with open(capture + '.prompts', 'a') as stream:
            stream.write(json.dumps(message['message']['content']) + '\n')
        with open(transcript, 'a') as stream:
            stream.write(json.dumps(message['message']['content']) + '\n')
            stream.flush()
            os.fsync(stream.fileno())
        # Like the native Claude 2.1.300: each prompt is queued and started, then init comes again.
        for state in ('queued', 'started'):
            lifecycle(message, state)
        if len(pending) == 1:
            # Like the native Claude 2.1.291 resuming a session: its title comes before init.
            send(dict(type='system', subtype='session_title_changed', title='fixture', session_id=session_id, uuid='00000000-0000-4000-8000-000000000106'))
            send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-000000000100', claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='default', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
            # Like the native Claude 2.1.291: system messages no SDK release declares yet.
            send(dict(type='system', subtype='dev_intent', session_id=session_id, uuid='00000000-0000-4000-8000-000000000104'))
            send(dict(type='system', subtype='future_subtype', session_id=session_id, uuid='00000000-0000-4000-8000-000000000105'))
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
        elif text == '/compact':
            serial += 1
            send(dict(type='system', subtype='compact_boundary', compact_metadata=dict(trigger='manual', pre_tokens=1),
                      session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12)))
            result(message)
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
        elif text.startswith('bulky:'):
            # A long turn whose answers are close to the largest event the host keeps.
            for _ in range(int(text[len('bulky:'):])):
                consume(message, dict(type='text', text='x' * 900_000))
            result(message)
        elif text == 'huge-tool-result':
            consume(message, dict(type='tool_use', id='toolu_fixture_huge', name='Bash', input=dict(command='cat huge.log')))
            output = 'x' * (5 * 1024 * 1024)
            serial += 1
            send(dict(type='user', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), parent_tool_use_id=None,
                      message=dict(role='user', content=[dict(type='tool_result', tool_use_id='toolu_fixture_huge', content=output)]),
                      tool_use_result=dict(stdout=output, stderr='', interrupted=False)))
            result(message)
        elif text.startswith('[From ') and len(pending) > 1:
            # Like the native Claude 2.1.300 with a turn open: a message that arrives meanwhile is
            # taken into that same turn, which then ends.
            consume(message)
            result(message)
        elif text.startswith('decide:'):
            scenario, _, argument = text[len('decide:'):].partition(' ')
            decide(message, scenario, argument)
        elif text in ('foreground-command', 'foreground-command-hold'):
            foreground_started(message)
            if text == 'foreground-command':
                foreground_ended('completed')
                result(message)
        elif text == 'background':
            # Like a native turn that starts an agent of its own: it takes the input, then the work starts.
            consume(message)
            send(dict(type='system', subtype='task_started', task_id='fixture-background', description='Fixture owned work', task_type='local_agent', uuid='00000000-0000-4000-8000-000000000101', session_id=session_id))
        else:
            if len(pending) > 1:
                send(dict(type='system', subtype='task_notification', task_id='fixture-background', status='completed', output_file='fixture-output', summary='Fixture work completed', uuid='00000000-0000-4000-8000-000000000102', session_id=session_id))
            result(message)
    elif message['type'] == 'control_response':
        decided(message)
    elif message['type'] == 'control_cancel_request':
        pass
    else:
        raise RuntimeError('Unexpected fixture message')
