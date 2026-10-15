import { createInterface } from 'node:readline';
import { writeFileSync, createReadStream } from 'node:fs';

const send = (value) => process.stdout.write(JSON.stringify(value) + '\n');
const scenario = process.env.FIXTURE_SCENARIO;
if (scenario === 'exit_barrier') {
  const release = createReadStream(null, { fd: 3 });
  release.once('data', () => process.exit(0));
}
const result = (overrides = {}) => ({ type: 'result', subtype: 'success', is_error: false, result: 'fixture complete', session_id: 'fixture-session', uuid: '00000000-0000-4000-8000-000000000001', duration_ms: 0, duration_api_ms: 0, num_turns: 1, total_cost_usd: 0, usage: {}, modelUsage: {}, permission_denials: [], ...overrides });
let active;
let turns = 0;
const retryInputs = [];
const lines = createInterface({ input: process.stdin });
lines.on('line', (line) => {
  const message = JSON.parse(line);
  if (message.type === 'control_request') {
    if (message.request.subtype === 'initialize') {
      writeFileSync(process.env.FIXTURE_CAPTURE, JSON.stringify({ args: process.argv.slice(2), initialize: message.request, home: process.env.HOME, config_dir: process.env.CLAUDE_CONFIG_DIR, startup_failure_results: process.env.CLAUDE_CODE_STARTUP_FAILURE_RESULTS }));
      send({ type: 'control_response', response: { subtype: 'success', request_id: message.request_id, response: { commands: [], models: [], account: {}, output_style: 'default' } } });
      send({ type: 'system', subtype: 'init', session_id: 'fixture-session', uuid: 'fixture-init', claude_code_version: '2.1.300-fixture', model: 'fixture', cwd: process.cwd(), tools: ['Bash'], mcp_servers: [], permissionMode: 'default', slash_commands: [], output_style: 'default', skills: [], plugins: [], apiKeySource: 'none' });
    } else if (message.request.subtype === 'interrupt') {
      send({ type: 'control_response', response: { subtype: 'success', request_id: message.request_id, response: { still_queued: [] } } });
      send(result({ is_error: false, result: 'interrupted', terminal_reason: 'aborted_tools' }));
    } else throw new Error(`Unsupported fake control ${message.request.subtype}`);
  } else if (message.type === 'user') {
    active = message;
    turns += 1;
    if (scenario === 'api_retry' || scenario === 'api_retry_connection') {
      retryInputs.push(message.uuid);
      if (turns === 1) send({ type: 'system', subtype: 'api_retry', attempt: 1, max_retries: 3, retry_delay_ms: 1000, error_status: scenario === 'api_retry' ? 401 : null, error: scenario === 'api_retry' ? 'authentication_failed' : 'server_error', ...(scenario === 'api_retry_connection' ? { no_response: { waited_ms: 10, retry_wait_ms: 20 } } : {}), uuid: '00000000-0000-4000-8000-000000000099', session_id: 'fixture-session' });
      else send(result({ user_message_uuid: message.uuid, user_message_uuids: retryInputs }));
      return;
    }
    if ((scenario === 'eof' || (scenario === 'multi_eof' && turns === 2))) { process.stdout.end(); lines.close(); process.stdin.destroy(); return; }
    if (scenario === 'permission' || scenario === 'cancel_permission') {
      send({ type: 'control_request', request_id: 'permission-request', request: { subtype: 'can_use_tool', tool_name: 'Bash', input: { command: 'fixture' }, tool_use_id: 'fixture-tool', mcp_server: { name: 'fixture-server', source: 'project' }, matched_ask_rule: { source: 'project', tool_name: 'Bash' } } });
      if (scenario === 'cancel_permission') send({ type: 'control_cancel_request', request_id: 'permission-request' });
    } else if (scenario === 'startup') send(result({ subtype: 'error_during_execution', is_error: true, errors: ['fixture startup'], startup_failure_reason: 'session_held_by_background' }));
    else if (scenario === 'logout') {
      send({ type: 'auth_status', isAuthenticating: true, output: ['fixture login in progress'], uuid: 'auth-progress', session_id: 'fixture-session' });
      send({ type: 'auth_status', isAuthenticating: false, output: [], error: 'fixture logged out', uuid: 'auth-error', session_id: 'fixture-session' });
      send({ type: 'assistant', error: 'authentication_failed', message: { role: 'assistant', content: [] }, uuid: 'assistant-error', session_id: 'fixture-session' });
      send(result({ is_error: true, result: 'fixture authentication failure', terminal_reason: 'api_error' }));
    } else if (scenario === 'interrupt' || scenario === 'exit_barrier') send({ type: 'system', subtype: 'status', status: 'compacting', uuid: 'ready', session_id: 'fixture-session' });
    else if (scenario === 'failed_delivery') send(result({ subtype: 'error_during_execution', is_error: true, errors: ['fixture failed delivery'], num_turns: 0, user_message_uuid: message.uuid }));
    else if (scenario === 'invalid_plural') send(result({ user_message_uuid: message.uuid, user_message_uuids: [message.uuid, 'not-a-uuid'] }));
    else if (scenario === 'duplicate_plural') send(result({ user_message_uuid: message.uuid, user_message_uuids: [message.uuid, message.uuid.toUpperCase()] }));
    else if (scenario === 'oversized_plural') send(result({ user_message_uuid: message.uuid, user_message_uuids: Array(65).fill(message.uuid) }));
    else if (scenario === 'missing_singular') send(result({ user_message_uuids: [message.uuid] }));
    else if (scenario === 'mismatched_singular') send(result({ user_message_uuid: '00000000-0000-4000-8000-000000000098', user_message_uuids: [message.uuid] }));
    else if (scenario === 'invalid_singular') send(result({ user_message_uuid: 'not-a-uuid', user_message_uuids: [message.uuid] }));
    else if (scenario === 'child_plural') {
      send({ type: 'assistant', parent_tool_use_id: 'fixture-parent', message: { role: 'assistant', content: [{ type: 'text', text: 'child consumption cannot release root send' }] }, uuid: 'fixture-child', session_id: 'fixture-session', user_message_uuid: message.uuid, user_message_uuids: [message.uuid] });
      send(result({ user_message_uuid: message.uuid }));
    }
    else if (scenario === 'assistant_consumed') {
      send({ type: 'assistant', message: { role: 'assistant', content: [{ type: 'text', text: 'fixture consumed' }] }, uuid: 'fixture-assistant', session_id: 'fixture-session', user_message_uuid: message.uuid, user_message_uuids: [message.uuid] });
      send(result({ user_message_uuid: message.uuid }));
    } else if (scenario === 'foreign_consumption') {
      send({ type: 'system', subtype: 'status', status: 'compacting', uuid: 'fixture-status', session_id: 'fixture-session', user_message_uuids: [message.uuid] });
      send(result({ user_message_uuid: message.uuid }));
    } else send(result({ user_message_uuid: message.uuid, ...(scenario === 'legacy_singular' ? {} : { user_message_uuids: [message.uuid] }) }));
  } else if (message.type === 'control_response') {
    writeFileSync(process.env.FIXTURE_DECISION, JSON.stringify(message));
    send(result({ user_message_uuid: active?.uuid }));
  } else throw new Error(`Unsupported fake message ${message.type}`);
});
lines.on('close', () => { if (scenario !== 'exit_barrier') process.stdout.end(); });
