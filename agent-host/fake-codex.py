#!/usr/bin/python3
import json, os, sys, subprocess, threading, pwd, re, stat
if os.environ.get('ALONDRA_FAKE_ARGV'):
    with open(os.environ['ALONDRA_FAKE_ARGV'], 'w') as target: json.dump(sys.argv[1:], target)
thread = '00000000-0000-4000-8000-000000000001'
turn = 'native-turn'
turn_number = 0
accepted_input = None
raw_enabled = False
child_pending = False
child_turn = 'child-turn-' + str(os.getpid())
mode = os.environ.get('ALONDRA_FAKE_MODE', 'complete')
if os.environ.get('ALONDRA_FAKE_MODE_FILE'):
    with open(os.environ['ALONDRA_FAKE_MODE_FILE']) as source: mode = source.read().strip()

def account_fixture():
    path = os.environ.get('ALONDRA_FAKE_ACCOUNT_FILE', os.path.abspath(__file__) + '.account.json')
    try:
        with open(path) as source: return json.load(source)
    except FileNotFoundError: return {}
def account_read(fixture):
    return fixture.get('accountRead', {'account': {'type': 'chatgpt', 'email': 'first@example.test', 'planType': 'plus'}, 'requiresOpenaiAuth': True})
def auth_status(fixture):
    account = account_read(fixture).get('account')
    method = None if account is None else 'apikey' if account.get('type') == 'apiKey' else 'chatgpt'
    return fixture.get('authStatus', {'authMethod': method, 'requiresOpenaiAuth': True, 'authToken': None})
def model_list(fixture):
    models = [{'id': model, 'model': model, 'displayName': model, 'isDefault': i == 0, 'hidden': False, 'supportedReasoningEfforts': [{'reasoningEffort': 'high', 'description': 'High'}], 'defaultReasoningEffort': 'high', 'inputModalities': ['text']} for i, model in enumerate(os.environ.get('ALONDRA_FAKE_MODELS', 'fixture-model').split(','))]
    return fixture.get('models', {'data': models, 'nextCursor': None})

rollout_lock = threading.Lock()
def rollout_path(native_thread):
    if not isinstance(native_thread, str) or not re.fullmatch(r'[A-Za-z0-9_-]{1,256}', native_thread):
        raise RuntimeError('Fixture native thread identity is invalid')
    home = os.environ.get('HOME')
    if not home or not os.path.isabs(home) or os.path.realpath(home) == os.path.realpath(pwd.getpwuid(os.getuid()).pw_dir):
        raise RuntimeError('The fake Codex requires an explicit temporary HOME')
    profile = os.environ.get('CODEX_HOME', os.path.join(home, '.codex'))
    if not os.path.isabs(profile) or os.path.commonpath([os.path.realpath(home), os.path.realpath(profile)]) != os.path.realpath(home):
        raise RuntimeError('The fake Codex profile must remain inside its temporary HOME')
    current = os.path.abspath(home)
    for component in [None, *os.path.relpath(profile, home).split(os.sep), 'sessions']:
        if component is not None:
            if component == '.': continue
            current = os.path.join(current, component)
            try: os.mkdir(current, 0o700)
            except FileExistsError: pass
        node = os.lstat(current)
        if not stat.S_ISDIR(node.st_mode) or node.st_uid != os.getuid():
            raise RuntimeError('The fake Codex refuses an unowned or linked session directory')
    path = os.path.join(current, 'rollout-fixture-' + native_thread + '.jsonl')
    try:
        descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
        with os.fdopen(descriptor, 'w') as target:
            target.write(json.dumps({'timestamp': '2026-01-01T00:00:00Z', 'type': 'session_meta', 'payload': {'id': native_thread}}) + '\n')
            target.flush()
            os.fsync(target.fileno())
    except FileExistsError:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor) as source:
            node = os.fstat(source.fileno())
            if not stat.S_ISREG(node.st_mode) or node.st_nlink != 1 or node.st_uid != os.getuid():
                raise RuntimeError('The fake Codex refuses an unowned session file')
            first = json.loads(source.readline())
            if first.get('type') != 'session_meta' or first.get('payload', {}).get('id') != native_thread:
                raise RuntimeError('The fake Codex session identity differs')
    return path

def rollout_record(native_thread, kind, payload):
    with rollout_lock:
        path = rollout_path(native_thread)
        descriptor = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'a') as target:
            node = os.fstat(target.fileno())
            if not stat.S_ISREG(node.st_mode) or node.st_nlink != 1 or node.st_uid != os.getuid():
                raise RuntimeError('The fake Codex refuses an unowned session append')
            target.write(json.dumps({'timestamp': '2026-01-01T00:00:00Z', 'type': kind, 'payload': payload}) + '\n')
            target.flush()
            os.fsync(target.fileno())

output_lock = threading.Lock()
def send(value):
    with output_lock: print(json.dumps(value), flush=True)
def note(method, params):
    send({'method': method, 'params': params})
def user_item(params):
    note('item/started', {'threadId': thread, 'turnId': turn, 'item': {'type': 'userMessage', 'id': 'user-' + turn, 'clientId': params.get('clientUserMessageId'), 'content': params.get('input', [])}})
def checkpoint_after_barrier(release, native_turn, ready):
    descriptor = os.open(release, os.O_RDWR)
    ready.set()
    os.read(descriptor, 1)
    os.close(descriptor)
    usage = {'totalTokens': 37, 'inputTokens': 37, 'cachedInputTokens': 0, 'outputTokens': 0, 'reasoningOutputTokens': 0}
    note('thread/tokenUsage/updated', {'threadId': thread, 'turnId': native_turn, 'tokenUsage': {'total': usage, 'last': usage, 'modelContextWindow': 32768}})
def finish():
    if os.environ.get('ALONDRA_FAKE_TOKEN_USAGE') == '1':
        total = {'totalTokens': 30 * turn_number, 'inputTokens': 20 * turn_number, 'cachedInputTokens': 0, 'outputTokens': 10 * turn_number, 'reasoningOutputTokens': 0}
        note('thread/tokenUsage/updated', {'threadId': thread, 'turnId': turn, 'tokenUsage': {'total': total, 'last': {'totalTokens': 30, 'inputTokens': 20, 'cachedInputTokens': 0, 'outputTokens': 10, 'reasoningOutputTokens': 0}, 'modelContextWindow': 32768}})
    note('item/agentMessage/delta', {'threadId': thread, 'turnId': turn, 'itemId': 'answer', 'delta': 'untrusted delta'})
    note('item/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'agentMessage', 'id': 'answer', 'text': os.environ.get('ALONDRA_FAKE_FINAL_TEXT', ''), 'phase': 'final_answer'}})
    note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'completed'}})
for line in sys.stdin:
    message = json.loads(line)
    if os.environ.get('ALONDRA_FAKE_TRANSCRIPT'):
        with open(os.environ['ALONDRA_FAKE_TRANSCRIPT'], 'a') as transcript:
            transcript.write(json.dumps(message) + '\n')
    fixture = account_fixture()
    if fixture.get('trace'):
        path = os.environ.get('ALONDRA_FAKE_ACCOUNT_FILE', os.path.abspath(__file__) + '.account.json')
        with open(path + '.calls', 'a') as target: target.write(json.dumps(message) + '\n')
    method = message.get('method')
    identity = message.get('id')
    if not method:
        if identity in [77, '77', 88, '88']:
            finish()
        continue
    if method == 'initialized': continue
    result = {}
    if method == 'config/read': result = fixture.get('configRead', {'config': {'mcp_servers': {name: {'command': '/untrusted'} for name in json.loads(os.environ.get('ALONDRA_FAKE_INHERITED_MCP_NAMES', '["inherited-danger"]'))}}})
    elif method in ['thread/start', 'thread/resume']:
        raw_enabled = message.get('params', {}).get('experimentalRawEvents') is True
        result = {'thread': {'id': thread, 'path': rollout_path(thread)}, 'model': message.get('params', {}).get('model') or 'fixture-model', 'modelProvider': 'openai'}
    elif method == 'account/read': result = account_read(fixture)
    elif method == 'getAuthStatus':
        if message.get('params') != {'includeToken': False, 'refreshToken': False}: raise RuntimeError('Account checks must not request tokens or refresh authentication')
        result = auth_status(fixture)
    elif method == 'account/rateLimits/read':
        if fixture.get('rateLimitsError'):
            send({'id': identity, 'error': {'code': -32603, 'message': 'Fixture usage metadata unavailable'}})
            continue
        result = fixture.get('rateLimits', {'accountId': 'fixture-workspace', 'ordinaryUsageAllowed': True, 'rateLimits': {'primary': {'usedPercent': 10, 'windowDurationMins': 300, 'resetsAt': 4102444800}, 'secondary': {'usedPercent': 20, 'windowDurationMins': 10080, 'resetsAt': 4102444800}}, 'rateLimitsByLimitId': None})
    elif method == 'model/list': result = model_list(fixture)
    elif method == 'thread/backgroundTerminals/list': result = {'data': [], 'nextCursor': None}
    elif method == 'thread/items/list': result = {'data': [{'turnId': child_turn, 'item': {'type': 'agentMessage', 'id': 'child-final', 'phase': 'final_answer', 'text': 'Managed native child public result'}}], 'nextCursor': None}
    elif method == 'thread/turns/list': result = {'data': [{'id': child_turn, 'status': 'completed', 'items': []}], 'nextCursor': None}
    elif method == 'thread/read':
        requested = message.get('params', {}).get('threadId')
        result = {'thread': {'id': requested, 'path': rollout_path(requested), 'turns': []}}
    elif method == 'mcpServerStatus/list':
        result = {'data': [{'name': 'alondra', 'runtimeStatus': 'connected'}], 'nextCursor': None}
        if child_pending:
            child_pending = False
            rollout_record('native-child', 'event_msg', {'type': 'task_complete', 'turn_id': child_turn})
            note('turn/completed', {'threadId': 'native-child', 'turn': {'id': child_turn, 'status': 'completed'}})
            send({'id': identity, 'result': result})
            note('item/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'subAgentActivity', 'id': 'child-completed', 'kind': 'completed', 'agentThreadId': 'native-child', 'agentPath': 'root/child', 'model': None, 'reasoningEffort': None}})
            continue
        if fixture.get('codeCellOnStatus') and turn_number == 1:
            send({'id': identity, 'result': result})
            note('rawResponseItem/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'custom_tool_call', 'namespace': 'functions', 'name': 'exec', 'call_id': 'pending-before-cell', 'input': 'await fixture_pending_cell();'}})
            note('rawResponseItem/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'custom_tool_call_output', 'call_id': 'pending-before-cell', 'output': [{'type': 'input_text', 'text': 'Script running with cell ID 17\nWall time 0.1 seconds\nOutput:\n'}]}})
            note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'completed'}})
            continue
        if os.environ.get('ALONDRA_FAKE_FINISH_ON_MCP_STATUS') == '1' and turn_number == 1 and mode == 'hold':
            send({'id': identity, 'result': result})
            finish()
            continue
        if accepted_input is not None:
            send({'id': identity, 'result': result})
            user_item(accepted_input)
            accepted_input = None
            finish()
            continue
    elif method == 'thread/backgroundTerminals/list': result = {'data': [{'processId': '42'}], 'nextCursor': None}
    elif method == 'turn/start':
        turn_mode = fixture.get('turnMode', mode)
        turn_number += 1
        turn = 'native-turn-' + str(turn_number)
        rollout_record(thread, 'turn_context', {'turn_id': turn})
        note('turn/started', {'threadId': thread, 'turn': {'id': turn, 'status': 'inProgress'}})
        send({'id': identity, 'result': {'turn': {'id': turn, 'status': 'inProgress'}}})
        if turn_mode == 'accepted_without_consumption' and turn_number == 1:
            accepted_input = message.get('params', {})
            continue
        user_item(message.get('params', {}))
        if fixture.get('checkpointRelease') and turn_number == 1:
            ready = threading.Event()
            threading.Thread(target=checkpoint_after_barrier, args=(fixture['checkpointRelease'], turn, ready), daemon=True).start()
            ready.wait()
        if fixture.get('codeCellOnStatus') and turn_number == 2:
            note('rawResponseItem/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'function_call', 'namespace': 'functions', 'name': 'wait', 'call_id': 'wait-existing-cell', 'arguments': json.dumps({'cell_id': '17'})}})
            note('rawResponseItem/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'function_call_output', 'call_id': 'wait-existing-cell', 'output': [{'type': 'input_text', 'text': 'Script completed\nWall time 0.1 seconds\nOutput:\n'}]}})
            finish()
            continue
        if turn_mode == 'permission' and turn_number == 1:
            send({'id': 88, 'method': 'item/commandExecution/requestApproval', 'params': {'threadId': thread, 'turnId': turn, 'itemId': 'pending-permission', 'command': 'alondra-fixture-needs-consent', 'cwd': os.getcwd(), 'reason': 'The fixture waits for explicit user permission', 'availableDecisions': ['accept', 'decline']}})
        elif turn_mode == 'blocked_command' and turn_number == 1:
            owned = fixture['ownedCommand']
            script = 'import os,sys; print(os.getpid(),flush=True); open(sys.argv[1]).read()'
            child = subprocess.Popen(['/usr/bin/python3', '-c', script, owned['release']], stdout=subprocess.PIPE, text=True)
            pid = int(child.stdout.readline().strip())
            with open(owned['pidFile'], 'w') as target: json.dump({'pid': pid}, target)
            command_turn = turn
            note('item/started', {'threadId': thread, 'turnId': command_turn, 'item': {'type': 'commandExecution', 'id': 'owned-command', 'command': 'fixture command waits on its explicit barrier', 'cwd': os.getcwd(), 'processId': str(pid), 'status': 'inProgress', 'aggregatedOutput': '', 'exitCode': None}})
            note('turn/completed', {'threadId': thread, 'turn': {'id': command_turn, 'status': 'completed'}})
            def completed_owned_command():
                code = child.wait()
                child.stdout.close()
                note('item/completed', {'threadId': thread, 'turnId': command_turn, 'item': {'type': 'commandExecution', 'id': 'owned-command', 'command': 'fixture command waits on its explicit barrier', 'cwd': os.getcwd(), 'processId': str(pid), 'status': 'completed', 'aggregatedOutput': 'The explicit child barrier was released', 'exitCode': code}})
            threading.Thread(target=completed_owned_command, daemon=True).start()
        elif turn_mode == 'batch_live_cell':
            call = {'type': 'custom_tool_call', 'namespace': 'functions', 'name': 'exec', 'call_id': 'batch-live-cell', 'input': 'await new Promise(() => {})', 'internal_chat_message_metadata_passthrough': {'turn_id': turn}}
            output = {'type': 'custom_tool_call_output', 'call_id': 'batch-live-cell', 'output': [{'type': 'input_text', 'text': 'Script running with cell ID 1\nWall time 0.0 seconds\nOutput:\n'}], 'internal_chat_message_metadata_passthrough': {'turn_id': turn}}
            for item in [call, output]:
                rollout_record(thread, 'response_item', item)
                if raw_enabled: note('rawResponseItem/completed', {'threadId': thread, 'turnId': turn, 'item': item})
            finish()
        elif turn_mode in ('bad_request', 'context_exceeded'):
            kind = 'badRequest' if turn_mode == 'bad_request' else 'contextWindowExceeded'
            note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'failed', 'error': {'message': 'The fixture native request was rejected', 'codexErrorInfo': kind}}})
        elif turn_mode == 'auth':
            note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'failed', 'error': {'message': 'The fixture native sign-in needs attention', 'codexErrorInfo': 'unauthorized'}}})
        elif turn_mode == 'quota':
            note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'failed', 'error': {'message': 'The fixture account quota is exhausted', 'codexErrorInfo': 'usageLimitExceeded'}}})
        elif turn_mode == 'child' and turn_number == 1:
            child_pending = True
            note('item/started', {'threadId': thread, 'turnId': turn, 'item': {'type': 'collabAgentToolCall', 'id': 'spawn-child', 'tool': 'spawnAgent', 'status': 'inProgress', 'senderThreadId': thread, 'receiverThreadIds': [], 'agentsStates': {}}})
            rollout_record('native-child', 'turn_context', {'turn_id': child_turn})
            note('turn/started', {'threadId': 'native-child', 'turn': {'id': child_turn, 'status': 'inProgress'}})
            note('item/completed', {'threadId': thread, 'turnId': turn, 'item': {'type': 'collabAgentToolCall', 'id': 'spawn-child', 'tool': 'spawnAgent', 'status': 'completed', 'senderThreadId': thread, 'receiverThreadIds': ['native-child'], 'agentsStates': {'native-child': {'status': 'running', 'message': None}}}})
            note('item/completed', {'threadId': 'native-child', 'turnId': child_turn, 'item': {'type': 'plan', 'id': 'child-plan', 'text': 'This child plan must never become the whole task plan'}})
            note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'completed'}})
        elif turn_mode == 'plan' and turn_number == 1:
            note('item/completed', {'threadId': thread, 'turnId': turn, 'item': {'type':'plan','id':'plan-item','text':'- [ ] Implement the change\n- [ ] Verify the result'}})
            note('turn/completed', {'threadId': thread, 'turn': {'id':turn,'status':'completed'}})
        elif turn_mode == 'question':
            params = {'threadId': thread, 'turnId': turn, 'itemId': 'question-item', 'questions': [{'id': 'language', 'header': 'Language', 'question': 'Which language?', 'options': [{'label': 'English', 'description': 'English text'}], 'isSecret': False, 'isOther': True}]}
            send({'id': 77, 'method': 'item/tool/requestUserInput', 'params': params})
            send({'id': 77, 'method': 'item/tool/requestUserInput', 'params': params})
        elif turn_mode != 'hold' or (os.environ.get('ALONDRA_FAKE_FINISH_ON_MCP_STATUS') == '1' and turn_number > 1): finish()
        continue
    elif method == 'turn/interrupt':
        note('turn/completed', {'threadId': thread, 'turn': {'id': turn, 'status': 'interrupted'}})
    send({'id': identity, 'result': result})
