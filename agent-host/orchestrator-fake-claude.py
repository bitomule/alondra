#!/usr/bin/python3
"""A native Claude stand-in for an orchestrator batch run through the SDK host.

It initializes like `claude --restricted` with Alondra's orchestrator MCP server connected,
records its arguments and every prompt it gets, and answers each prompt as its first marker says:

  fixture:hold              answers, then holds the turn until the test writes to FIXTURE_RELEASE,
                            after telling it so through FIXTURE_HELD
  fixture:tool-then-exit    calls an Alondra tool that changes something, then dies without a result
  fixture:denied            calls an Alondra tool and ends with it refused
  fixture:limited           usage runs out before it answers
  fixture:no-conversation   the resumed conversation does not exist
  fixture:exit-before-init  dies before it initializes
  fixture:another-input     ends with the result of a prompt that is not this one

Any other prompt is answered and ends with its result.
"""
import json
import os
import sys
import time

capture = os.environ['FIXTURE_CAPTURE']
arguments = sys.argv[1:]


def flag(name):
    for at, argument in enumerate(arguments):
        if argument.startswith(name + '='):
            return argument.split('=', 1)[1]
        if argument == name and at + 1 < len(arguments):
            return arguments[at + 1]
    return None


resumed = flag('--resume')
session_id = resumed or flag('--session-id')
serial = 0
server = 'alondra-orchestrator'
tools = ['Skill', 'Read', 'Glob', 'Grep'] + ['mcp__%s__%s' % (server, name) for name in ('fleet', 'ask', 'create_task', 'close_request', 'settle_suggestion')]


def uuid():
    global serial
    serial += 1
    return '00000000-0000-4000-8000-' + str(serial).zfill(12)


def send(value):
    sys.stdout.write(json.dumps(value, ensure_ascii=False) + '\n')
    sys.stdout.flush()


def lifecycle(message, state):
    send(dict(type='command_lifecycle', command_uuid=message['uuid'], state=state, session_id=session_id, uuid=uuid()))


def init():
    send(dict(type='system', subtype='init', session_id=session_id, uuid=uuid(), claude_code_version='2.1.300-fixture',
              model=flag('--model'), cwd=os.getcwd(), tools=tools, mcp_servers=[dict(name=server, status='connected')],
              permissionMode=flag('--permission-mode'), slash_commands=[], output_style='default', skills=[], plugins=[],
              apiKeySource='none'))


def assistant(message, content, **extra):
    send(dict(type='assistant', session_id=session_id, uuid=uuid(), parent_tool_use_id=None,
              message=dict(id='msg_' + str(serial), type='message', role='assistant', model=flag('--model'), content=content,
                           stop_reason=None, stop_sequence=None, usage=dict(input_tokens=25, output_tokens=1)),
              user_message_uuid=message['uuid'], user_message_uuids=[message['uuid']], **extra))


def result(message, input_id=None, **extra):
    value = dict(type='result', subtype='success', is_error=False, result='Batch handled', session_id=session_id, uuid=uuid(),
                 duration_ms=0, duration_api_ms=0, num_turns=1, total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
                 terminal_reason='completed', user_message_uuid=input_id or message['uuid'], user_message_uuids=[input_id or message['uuid']])
    value.update(extra)
    send(value)
    lifecycle(message, 'completed')


for raw in sys.stdin:
    message = json.loads(raw)
    if message['type'] == 'control_request':
        subtype = message['request']['subtype']
        if subtype == 'initialize':
            with open(capture, 'w') as stream:
                json.dump(dict(args=arguments, env_keys=sorted(os.environ)), stream)
            send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'],
                                                              response=dict(commands=[], models=[], account={}, output_style='default'))))
        elif subtype == 'interrupt':
            send(dict(type='control_response', response=dict(subtype='success', request_id=message['request_id'], response=dict(still_queued=[]))))
        else:
            raise RuntimeError('Unexpected fixture control request')
        continue
    if message['type'] != 'user':
        raise RuntimeError('Unexpected fixture message')
    text = message['message']['content']
    with open(capture + '.prompts', 'a') as stream:
        stream.write(json.dumps(text) + '\n')
    if 'fixture:exit-before-init' in text:
        sys.exit(3)
    for state in ('queued', 'started'):
        lifecycle(message, state)
    if 'fixture:no-conversation' in text and resumed:
        send(dict(type='result', subtype='error_during_execution', is_error=True, session_id=session_id, uuid=uuid(),
                  duration_ms=0, duration_api_ms=0, num_turns=0, total_cost_usd=0, usage={}, modelUsage={}, permission_denials=[],
                  errors=['No conversation found with session ID: %s' % session_id]))
        sys.exit(1)
    init()
    if 'fixture:limited' in text:
        send(dict(type='rate_limit_event', session_id=session_id, uuid=uuid(),
                  rate_limit_info=dict(status='rejected', resetsAt=int(time.time()) + 3600, rateLimitType='five_hour')))
        assistant(message, [dict(type='text', text="You've hit your limit")], error='rate_limit')
        result(message, subtype='error_during_execution', is_error=True, result="You've hit your limit", terminal_reason='api_error')
        continue
    if 'fixture:tool-then-exit' in text:
        assistant(message, [dict(type='tool_use', id='toolu_fixture', name='mcp__%s__create_task' % server, input=dict(title='Fixture'))])
        sys.exit(1)
    if 'fixture:denied' in text:
        assistant(message, [dict(type='tool_use', id='toolu_fixture', name='mcp__%s__create_task' % server, input=dict(title='Fixture'))])
        result(message, permission_denials=[dict(tool_name='mcp__%s__create_task' % server, tool_use_id='toolu_fixture', tool_input={})])
        continue
    assistant(message, [dict(type='text', text='Handling the batch')])
    if 'fixture:hold' in text:
        with open(os.environ['FIXTURE_HELD'], 'w') as held:
            held.write('held\n')
        with open(os.environ['FIXTURE_RELEASE']) as release:
            release.read(1)
        result(message)
    elif 'fixture:another-input' in text:
        result(message, input_id='00000000-0000-4000-8000-0000000000ff')
    else:
        result(message)
