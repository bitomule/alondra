#!/usr/bin/python3
import hashlib
import json
import os
import signal
import subprocess
import sys
import time

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
transcript = capture + '.transcript-' + (session_id or 'unsaved')
profile = next((os.environ[key] for key in ['CLAUDE_CONFIG_DIR', 'CLAUDE_HOME'] if os.environ.get(key, '').strip()), os.path.join(os.environ['HOME'], '.claude'))
project = ''.join(character if character.isascii() and character.isalnum() else '-' for character in os.getcwd())
native_transcript = os.path.join(profile, 'projects', project, (session_id or 'unsaved') + '.jsonl')
history = sum(1 for _ in open(transcript)) if os.path.exists(transcript) else 0
if resumed and not history:
    raise RuntimeError('No conversation found with session ID: ' + resumed)
with open(capture + '.starts', 'a') as stream:
    stream.write(json.dumps(dict(pid=os.getpid(), session=session_id, resume=resumed, history=history, cwd=os.getcwd(), config=os.environ.get('CLAUDE_CONFIG_DIR'))) + '\n')
    stream.flush()
    os.fsync(stream.fileno())
# Like a command Claude runs, it creates a folder and a file under the umask it inherited.
if 'FIXTURE_CREATES' in os.environ:
    subprocess.run(['/bin/sh', '-c', 'mkdir "$1" && : > "$1/file"', 'sh', os.environ['FIXTURE_CREATES']], check=True)
pending = []
# Each decision Claude asked for and still waits on, by request id.
asked = {}
decisions = 0
# The SDK's hook callbacks by event, from its initialize request; and the tool calls each waits on.
hooks = {}
hooked = {}
# Message uuids are unique across processes, as the native ones are: a resumed session never repeats them.
serial = (os.getpid() % 100000) * 1000000
# The API refuses every prompt, with a 401 for an expired sign-in or a 529 when it is overloaded,
# until the test marks it recovered with a file next to the capture.
refused = None
# Like a Claude whose turn never stops: it accepts the interrupt and keeps going, and only SIGKILL
# ends it.
stubborn = False
# Servers still connecting for this many status reads, as the native Claude reports them right after it starts.
connecting = int(os.environ.get('FIXTURE_MCP_CONNECTING', '0'))

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
            429: ('rate_limit', "You've hit your limit"),
            529: ('server_error', 'API Error: 529 Overloaded')}
# When the session window a 429 rejects resets, in Unix seconds: far ahead, so it never resets in a test.
LIMIT_RESETS_AT = 4102444800

SMALL_USAGE = dict(input_tokens=1, output_tokens=1, cache_creation_input_tokens=0, cache_read_input_tokens=0)
# What the API reports for a long conversation whose request read most of it from the prompt cache
# and wrote the rest for an hour.
LARGE_USAGE = dict(input_tokens=3, output_tokens=400, cache_creation_input_tokens=2000, cache_read_input_tokens=248000,
                   cache_creation=dict(ephemeral_5m_input_tokens=0, ephemeral_1h_input_tokens=2000))

def assistant(message, error=None, text='Fixture answer', usage=SMALL_USAGE):
    global serial
    serial += 1
    value = dict(type='assistant', session_id=session_id,
                 uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                 parent_tool_use_id=None,
                 message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant',
                              model='fixture', content=[dict(type='text', text=text)],
                              stop_reason=None, stop_sequence=None, usage=usage),
                 user_message_uuid=message['uuid'],
                 user_message_uuids=[entry['uuid'] for entry in pending])
    if error:
        value['error'] = error
    send(value)

def refuse(message, status):
    global serial
    error, text = REFUSALS[status]
    assistant(message, error=error, text=text)
    if status == 429:
        serial += 1
        send(dict(type='rate_limit_event', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                  rate_limit_info=dict(status='rejected', rateLimitType='five_hour', utilization=1.0, resetsAt=LIMIT_RESETS_AT)))
    serial += 1
    send(dict(type='result', subtype='success', is_error=True, api_error_status=status,
              session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
              duration_ms=0, duration_api_ms=0, num_turns=1, total_cost_usd=0, usage={}, modelUsage={},
              permission_denials=[], result=text,
              terminal_reason='blocking_limit' if status == 429 else 'api_error', stop_reason=None,
              user_message_uuid=message['uuid'], user_message_uuids=[entry['uuid'] for entry in pending]))
    for entry in pending:
        lifecycle(entry, 'completed')
    pending.clear()

def result(message, failed=False, subtype=None, model_usage=None, hold_completion=False):
    global serial
    serial += 1
    value = dict(type='result', subtype=subtype or ('error_during_execution' if failed else 'success'), is_error=failed or subtype is not None,
                 session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                 duration_ms=0, duration_api_ms=0, num_turns=0 if failed else 1,
                 total_cost_usd=0, usage={}, modelUsage=model_usage or {}, permission_denials=[],
                 user_message_uuid=message['uuid'])
    if failed:
        value['errors'] = ['Fixture delivery failed before consumption']
    elif subtype:
        value['errors'] = ['Fixture turn reached its limit']
    else:
        value.update(result='Fixture result', terminal_reason='completed', user_message_uuids=[entry['uuid'] for entry in pending])
    send(value)
    if hold_completion:
        with open(os.environ['FIXTURE_COMPLETION_GATE']) as gate:
            gate.readline()
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

# Like the native Claude whose background subagent hands its report back after the turn ended: it
# starts a turn of its own, with no prompt of Alondra's. Set by `own-turn-plan`, it starts as the
# next prompt arrives, which waits queued behind it.
own_turn = None
LONG_PLAN = '# Own plan\n\n' + '\n'.join('- [ ] Point ' + str(n) for n in range(1, 42))

def own_turn_plan(plan):
    global serial, decisions
    decisions += 1
    request_id = 'decision-' + str(decisions)
    tool_use_id = 'toolu_fixture_' + str(decisions)
    plan_file = capture + '.plan.md'
    with open(plan_file, 'w') as stream:
        stream.write(plan)
    serial += 1
    send(dict(type='assistant', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), parent_tool_use_id=None,
              message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant', model='fixture',
                           content=[dict(type='tool_use', id=tool_use_id, name='ExitPlanMode', input={})],
                           stop_reason=None, stop_sequence=None, usage=SMALL_USAGE)))
    asked[request_id] = dict(message=None, tool_name='ExitPlanMode', tool_use_id=tool_use_id, cancelled=False, plan=plan)
    send(dict(type='control_request', request_id=request_id,
              request=dict(subtype='can_use_tool', tool_name='ExitPlanMode', input=dict(plan=plan, planFilePath=plan_file), tool_use_id=tool_use_id)))

def own_turn_started(message):
    global serial, own_turn
    own_turn = message
    lifecycle(message, 'queued')
    serial += 1
    send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='plan', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
    own_turn_plan(LONG_PLAN)

def own_turn_ended():
    global serial, own_turn
    queued, own_turn = own_turn, None
    serial += 1
    send(dict(type='result', subtype='success', is_error=False, session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
              duration_ms=0, duration_api_ms=0, num_turns=1, total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
              result='Fixture own turn', terminal_reason='completed', user_message_uuid='00000000-0000-4000-8000-' + str(serial + 1).zfill(12)))
    serial += 1
    lifecycle(queued, 'started')
    consume(queued)
    result(queued)

# Like the native Claude 2.1.300 with a Bash command it left running in the background: a real
# process in the host's process group, whose PID is kept next to the capture. The command ends once
# the test writes a line to the FIFO beside it; its notification, which names no run, wakes a turn
# of Claude's own that opens with init, takes it up and ends, and the node rests.
def background_command(message):
    global serial
    release = capture + '.release'
    if not os.path.exists(release):
        os.mkfifo(release)
    command = subprocess.Popen(['/bin/sh', '-c', 'read line < "$1"', 'sh', release], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL)
    with open(capture + '.background', 'w') as stream:
        stream.write(str(command.pid))
        stream.flush()
        os.fsync(stream.fileno())
    consume(message, dict(type='tool_use', id='toolu_fixture_background', name='Bash', input=dict(command='read line < release', run_in_background=True)))
    serial += 1
    send(dict(type='system', subtype='task_started', task_id='fixture-command', tool_use_id='toolu_fixture_background',
              description='read line < release', task_type='local_bash', is_backgrounded=True, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), session_id=session_id))
    result(message)
    def ended():
        global serial
        status = 'completed' if command.wait() == 0 else 'failed'
        serial += 1
        send(dict(type='system', subtype='task_notification', task_id='fixture-command', tool_use_id='toolu_fixture_background', status=status,
                  output_file='fixture-output', summary='read line < release', uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), session_id=session_id))
        serial += 1
        send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='default', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
        serial += 1
        send(dict(type='assistant', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), parent_tool_use_id=None,
                  message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant', model='fixture',
                               content=[dict(type='text', text='The command ended')], stop_reason=None, stop_sequence=None, usage=SMALL_USAGE)))
        serial += 1
        send(dict(type='result', subtype='success', is_error=False, session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                  duration_ms=0, duration_api_ms=0, num_turns=1, total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
                  result='The command ended', terminal_reason='completed', origin=dict(kind='task-notification', producer='session-task')))
    import threading
    threading.Thread(target=ended, daemon=True).start()

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
    folder = os.getcwd()
    if scenario.startswith('bash@'):
        # Like a native Claude whose shell moved: `decide:bash@<folder> <command>` runs from <folder>.
        scenario, folder = 'bash', scenario[len('bash@'):]
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
    # Like the native Claude: a call's PreToolUse hooks run, and are answered, before it asks.
    callbacks = [callback for matcher in hooks.get('PreToolUse', []) if matcher.get('matcher') in (None, tool) for callback in matcher.get('hookCallbackIds', [])]
    if tool is not None and callbacks:
        hook_id = 'hook-' + str(decisions)
        hooked[hook_id] = dict(type='control_request', request_id=request_id, request=request)
        send(dict(type='control_request', request_id=hook_id, request=dict(subtype='hook_callback', callback_id=callbacks[0], tool_use_id=tool_use_id,
                  input=dict(session_id=session_id, transcript_path='', cwd=folder, hook_event_name='PreToolUse', tool_name=tool, tool_input=tool_input, tool_use_id=tool_use_id))))
        return
    send(dict(type='control_request', request_id=request_id, request=request))

def decided(message):
    global serial
    response = message['response']
    waiting = hooked.pop(response['request_id'], None)
    if waiting is not None:
        send(waiting)
        return
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
    if entry['message'] is None:
        # Claude's own turn: a plan refused for its length is shortened and asked again.
        if answer.get('behavior') != 'allow' and entry['plan'] == LONG_PLAN:
            own_turn_plan(FIXTURE_PLAN)
        else:
            own_turn_ended()
        return
    result(entry['message'])

for raw in sys.stdin:
    message = json.loads(raw)
    if message['type'] == 'control_request':
        subtype = message['request']['subtype']
        if subtype == 'initialize':
            hooks = message['request'].get('hooks') or {}
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
            if stubborn:
                continue
            if foreground:
                foreground_ended('stopped')
            if pending:
                result(pending[-1], failed=True)
        elif subtype in ('set_model', 'set_permission_mode'):
            control(message, {})
        elif subtype == 'mcp_status':
            remote = dict(name='linear', status='pending' if connecting > 0 else 'needs-auth', scope='user', config=dict(type='http', url='https://mcp.linear.app/mcp?token=secret'))
            connecting -= 1
            control(message, dict(mcpServers=[dict(name='fixture', status='connected')] + ([remote] if 'FIXTURE_MCP_CONNECTING' in os.environ else [])))
        elif subtype == 'mcp_reconnect':
            if message['request'].get('serverName') == 'fixture':
                control(message, {})
            else:
                control_error(message, 'Server not found: ' + str(message['request'].get('serverName')))
        elif subtype == 'mcp_clear_auth':
            if message['request'].get('serverName') == 'linear':
                control(message, {})
            else:
                control_error(message, 'Cannot clear auth for server type "stdio"')
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
        os.makedirs(os.path.dirname(native_transcript), mode=0o700, exist_ok=True)
        with open(native_transcript, 'a') as stream:
            stream.write(json.dumps(dict(type='user', sessionId=session_id, cwd=os.getcwd(), uuid=message.get('uuid'), message=message['message'])) + '\n')
            stream.flush()
            os.fsync(stream.fileno())
        text = message['message']['content']
        if text.endswith('complete-unstarted'):
            # Like the native Claude finishing a prompt whose uuid its transcript already holds: it
            # completes it without starting it, and no turn ever answers it.
            pending.remove(message)
            for state in ('queued', 'completed'):
                lifecycle(message, state)
            continue
        # Like a native Claude with nothing more to do: it answers Alondra's wake after an empty turn
        # with another turn that ends the same way, whatever the next prompt is scripted to do.
        wake = text.startswith('Your turn ended without a result') and len(pending) == 1
        if own_turn == 'next' and not wake:
            own_turn_started(message)
            continue
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
        if wake:
            result(message, hold_completion='FIXTURE_COMPLETION_GATE' in os.environ and not resumed)
            continue
        if text.endswith('signed-out'):
            refused = 401
        elif text.endswith('overloaded'):
            refused = 529
        elif text.endswith('rate-limited'):
            refused = 429
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
        elif text.endswith('stall-before-starting') and not resumed:
            # Like a native Claude busy with something else: the Claude that started the session
            # holds the prompt in its transcript but starts no turn for it until the host's process
            # group is killed; a Claude that resumes the session takes the same prompt up normally,
            # however far the first one got before the kill.
            time.sleep(3600)
        elif text == 'failed-delivery':
            result(message, failed=True)
        elif text == 'max-turns':
            consume(message)
            result(message, subtype='error_max_turns')
        elif text == 'consume-and-hold':
            consume(message)
        elif os.path.exists(capture + '.hold'):
            # Like the native Codex of 2026-10-27, resumed into a long turn: it takes the first prompt
            # and keeps working, and a message that arrives meanwhile waits unread in that turn.
            if len(pending) == 1:
                consume(message)
        elif text.endswith('rate-limited-twice'):
            # Like the native Claude whose subagent hands its report back after the usage limit
            # ended the turn: it opens a turn of its own, which the limit ends too.
            refuse(message, 429)
            serial += 1
            send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='default', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
            serial += 1
            send(dict(type='assistant', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), parent_tool_use_id=None, error='rate_limit',
                      message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant', model='fixture',
                                   content=[dict(type='text', text=REFUSALS[429][1])], stop_reason=None, stop_sequence=None, usage=SMALL_USAGE)))
            serial += 1
            send(dict(type='result', subtype='success', is_error=True, api_error_status=429, session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12),
                      duration_ms=0, duration_api_ms=0, num_turns=1, total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
                      result=REFUSALS[429][1], terminal_reason='api_error', stop_reason=None,
                      origin=dict(kind='task-notification', producer='session-task')))
        elif text == 'own-turn-later':
            # The turn ends; once the test drops a file next to the capture, Claude starts a turn
            # of its own, as when a subagent hands its report back while the task rests.
            consume(message)
            result(message)
            def later():
                global serial
                while not os.path.exists(capture + '.own-turn'):
                    time.sleep(0.05)
                serial += 1
                send(dict(type='system', subtype='init', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), claude_code_version='2.1.300-fixture', model='fixture', cwd=os.getcwd(), tools=['Bash'], mcp_servers=[], permissionMode='default', slash_commands=[], output_style='default', skills=[], plugins=[], apiKeySource='none'))
                serial += 1
                send(dict(type='assistant', session_id=session_id, uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), parent_tool_use_id=None,
                          message=dict(id='msg_fixture_' + str(serial), type='message', role='assistant', model='fixture',
                                       content=[dict(type='text', text='The report is in')], stop_reason=None, stop_sequence=None, usage=SMALL_USAGE)))
            import threading
            threading.Thread(target=later, daemon=True).start()
        elif text == 'own-turn-plan':
            consume(message)
            result(message)
            own_turn = 'next'
        elif text == 'ignore-interrupt':
            stubborn = True
            signal.signal(signal.SIGTERM, signal.SIG_IGN)
            consume(message)
        elif text == 'large-context':
            assistant(message, usage=LARGE_USAGE)
            result(message, model_usage={'fixture': dict(inputTokens=3, outputTokens=400, contextWindow=1_000_000)})
        elif text == 'large-context-unsized':
            # Claude sized only a subagent's model, never the one the conversation runs on.
            assistant(message, usage=LARGE_USAGE)
            result(message, model_usage={'fixture-subagent': dict(inputTokens=1, outputTokens=1, contextWindow=200_000)})
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
        elif text.endswith('background-shell') or 'background-shell\n' in text:
            # Like the native Claude running a Bash call with run_in_background: the command runs as
            # its child, the task starts, and the turn ends saying it goes on once the command ends.
            # Its notification would come only from this process: nothing outside it reports back.
            consume(message, dict(type='tool_use', id='toolu_fixture_background_shell', name='Bash',
                                  input=dict(command='bazel test //app:snapshots', run_in_background=True)))
            child = subprocess.Popen(['/bin/sleep', '600'])
            with open(capture + '.background', 'a') as stream:
                stream.write(str(child.pid) + '\n')
                stream.flush()
                os.fsync(stream.fileno())
            serial += 1
            send(dict(type='system', subtype='task_started', task_id='bfixture1', tool_use_id='toolu_fixture_background_shell',
                      description='bazel test //app:snapshots', task_type='local_bash', is_backgrounded=True,
                      uuid='00000000-0000-4000-8000-' + str(serial).zfill(12), session_id=session_id))
            result(message)
        elif text.endswith('background-command'):
            background_command(message)
        elif text.startswith('[From ') and len(pending) > 1:
            # Like the native Claude 2.1.300 with a turn open: a message that arrives meanwhile is
            # taken into that same turn, which then ends.
            consume(message)
            result(message)
        elif text.startswith('decide:') or (text.startswith('[From ') and text.partition('] ')[2].startswith('decide:')):
            # What the user tells the node arrives as `[From Alice · 09:00] decide:…`.
            said = text if text.startswith('decide:') else text.partition('] ')[2]
            scenario, _, argument = said[len('decide:'):].partition(' ')
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
while stubborn:
    time.sleep(3600)
