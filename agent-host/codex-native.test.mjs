import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, existsSync, rmSync } from 'node:fs';
import { tmpdir, userInfo } from 'node:os';
import { gunzipSync } from 'node:zlib';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { CodexSession } from './codex-session.js';

function names(tools, result = []) {
  for (const tool of Array.isArray(tools) ? tools : []) {
    if (typeof tool.name === 'string') result.push(tool.name);
    names(tool.tools, result);
  }
  return result;
}
function inventory(request) {
  const offered = [];
  const visit = (value) => {
    if (Array.isArray(value)) { for (const item of value) visit(item); return; }
    if (value === null || typeof value !== 'object') return;
    for (const [key, child] of Object.entries(value)) {
      if (key === 'tools' || key === 'additional_tools') names(child, offered);
      else visit(child);
    }
  };
  visit(request);
  return [...new Set(offered)];
}

const RESOURCE_CAPABILITIES = ['list_mcp_resources', 'list_mcp_resource_templates', 'read_mcp_resource'];
const ROLE_READS = ['glob_files', 'read_file', 'read_skill', 'search_files'];
const ROLE_CAPABILITIES = [...RESOURCE_CAPABILITIES, ...ROLE_READS.map((name) => `alondra.${name}`)];
function capability(name) {
  for (const key of RESOURCE_CAPABILITIES) if ([key, `functions__${key}`, `functions_${key}`, `functions.${key}`].includes(name)) return key;
  for (const key of ROLE_READS) if ([`mcp__alondra__${key}`, `mcp__alondra_${key}`, `mcp__alondra.${key}`].includes(name)) return `alondra.${key}`;
  if (['clock__curr_time', 'clock_curr_time', 'clock.curr_time'].includes(name)) return 'clock.curr_time';
  return `FORBIDDEN:${name}`;
}
function outputText(value) {
  if (typeof value === 'string') return value;
  if (Array.isArray(value)) return value.map(outputText).join('\n');
  if (value !== null && typeof value === 'object') return Object.values(value).map(outputText).join('\n');
  return '';
}
function runtimeProof(calls, resourceUrl) {
  return `const witness = { names: ALL_TOOLS.map(tool => tool.name).sort(), keys: Object.keys(tools).sort(), globals: {}, imports: [], images: [], attempts: [] };
for (const key of ['process','require','module','Deno','Bun','fetch','WebSocket','XMLHttpRequest']) witness.globals[key] = typeof globalThis[key];
witness.constructorProcess = Function('return typeof process')();
for (const name of ${JSON.stringify(['node:fs', 'node:child_process', 'file:///etc/passwd', resourceUrl])}) {
  try { await import(name); witness.imports.push({name, ok:true}); } catch(error) { witness.imports.push({name, ok:false, error:String(error).slice(0,160)}); }
}
for (const uri of ${JSON.stringify(['file:///etc/passwd', resourceUrl])}) {
  try { image(uri); witness.images.push({uri, ok:true}); } catch(error) { witness.images.push({uri, ok:false, error:String(error).slice(0,160)}); }
}
for (const [index, [name, args]] of ${JSON.stringify(calls.slice(0, 15))}.entries()) {
  try {
    const options = index < 8 ? [name] : [name, 'functions__'+name, 'functions_'+name, 'functions.'+name];
    const matches = ALL_TOOLS.filter(tool => options.includes(tool.name));
    const key = index < 8 ? name : matches.length === 1 ? matches[0].name : undefined;
    if (!key) throw new Error('Expected one native resource helper: '+name);
    witness.attempts.push({index, name, ok:true, result:await tools[key](args)});
  } catch(error) { witness.attempts.push({index, name, ok:false, error:String(error).slice(0,160)}); }
}
const matches = ALL_TOOLS.filter(tool => ['mcp__alondra__read_file','mcp__alondra_read_file','mcp__alondra.read_file'].includes(tool.name));
if (matches.length !== 1) throw new Error('Expected exactly one actual Alondra read_file, got '+matches.length);
witness.legitimate = await tools[matches[0].name]({path:'fixture.txt'});
text('ALONDRA_RUNTIME_WITNESS:'+JSON.stringify(witness));`;
}

function sse(response, items, id) {
  response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' });
  const send = (type, value) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...value })}\n\n`);
  send('response.created', { response: { id, object: 'response', status: 'in_progress', output: [] } });
  for (let index = 0; index < items.length; index++) {
    send('response.output_item.added', { output_index: index, item: items[index] });
    send('response.output_item.done', { output_index: index, item: items[index] });
  }
  send('response.completed', { response: { id, object: 'response', status: 'completed', output: items, usage: { input_tokens: 16, output_tokens: 4, total_tokens: 20, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } } } });
  response.end();
}

test('native Codex has no mutating orchestrator tools and rejects unavailable forced calls', { timeout: 60000 }, async (t) => {
  const nativeExecutable = process.env.CODEX_NATIVE_EXECUTABLE;
  assert(nativeExecutable, 'This opt-in target requires CODEX_NATIVE_EXECUTABLE; it never discovers the user runtime itself');
  const home = mkdtempSync(join(tmpdir(), 'alondra-codex-native-'));
  const codeHome = join(home, 'codex-home');
  const mcpScript = join(home, 'alondra-mcp');
  writeFileSync(mcpScript, readFileSync(resolve('agent-host/fake-codex-mcp.py')), { mode: 0o700 });
  const profilePath = join(home, 'native.sb');
  const realHome = JSON.stringify(userInfo().homedir);
  writeFileSync(profilePath, `(version 1)(allow default)(deny file-read* (subpath ${realHome}))(deny file-write* (subpath ${realHome}))(deny mach-lookup (global-name "com.apple.securityd"))(deny mach-lookup (global-name "com.apple.securityd.xpc"))(deny network*)(allow network-inbound (local ip "localhost:*"))(allow network-outbound (remote ip "localhost:*"))`);
  const executable = join(home, 'native-codex');
  const quote = (value) => "'" + value.replaceAll("'", "'\\''") + "'";
  writeFileSync(executable, `#!/bin/sh\nexec /usr/bin/sandbox-exec -f ${quote(profilePath)} ${quote(nativeExecutable)} "$@"\n`, { mode: 0o700 });
  const requests = []; const exchanges = []; const observed = []; const waiters = []; const rpcErrors = [];
  const snapshot = () => ({ rpcErrors, modelRequests: requests.length, exchanges: exchanges.slice(-24), observations: observed.slice(-32).map((event) => event.type === 'sdk' ? { type: event.type, method: event.payload.method, code: event.payload.params?.code, nativeStatus: event.payload.params?.turn?.status } : { type: event.type }) });
  const wake = () => { for (const notify of waiters.splice(0)) notify(); };
  t.signal.addEventListener('abort', wake, { once: true });
  let resourceFetches = 0;
  const server = createServer(async (request, response) => {
    exchanges.push({ method: request.method, path: request.url });
    const chunks = [];
    for await (const chunk of request) chunks.push(chunk);
    const encoded = Buffer.concat(chunks);
    if (request.method === 'POST' && request.url.split('?')[0] !== '/v1/responses') { response.writeHead(204); response.end(); return; }
    const body = chunks.length ? JSON.parse(request.headers['content-encoding'] === 'gzip' ? gunzipSync(encoded) : encoded) : {};
    if (request.url === '/forbidden-resource') { resourceFetches++; response.end('ALONDRA_BOUNDARY_SECRET'); return; }
    if (request.method === 'GET') {const account=request.url.includes('accounts/check');response.writeHead(account?200:404,{'content-type':'application/json'});response.end(JSON.stringify(account?{accounts:[{id:'fixture-workspace',plan_type:'plus',workspace_backend_origin:'https://chatgpt.com',account_routing_override:'NO_CONSTRAINT'}],account_ordering:['fixture-workspace'],default_account_id:'fixture-workspace'}:{error:'No remote model catalog in this fixture'}));return;}
    requests.push(body);
    if (requests.length === 1) {
      const sentinel = join(home, 'forbidden-effect');
      const calls = [
        ['exec_command', { cmd: `touch '${sentinel}'` }],
        ['apply_patch', { patch: `*** Begin Patch\n*** Add File: ${sentinel}\n+forbidden\n*** End Patch` }],
        ['exec', { code: `await tools.exec_command({cmd: "touch ${sentinel}"})` }],
        ['spawn_agent', { message: `Create ${sentinel}` }],
        ['create_goal', { objective: 'Forbidden native goal' }],
        ['update_goal', { status: 'complete' }],
        ['get_goal', {}],
        ['request_user_input', { questions: [{ id: 'forbidden', header: 'Question', question: 'Forbidden native question', options: [] }] }],
        ['list_mcp_resources', {}],
        ['list_mcp_resource_templates', {}],
        ['read_mcp_resource', { server: 'alondra', uri: 'file:///etc/passwd' }],
        ['read_mcp_resource', { server: 'alondra', uri: `http://127.0.0.1:${server.address().port}/forbidden-resource` }],
        ['read_mcp_resource', { server: 'inherited-user', uri: 'file:///etc/passwd' }],
        ['read_mcp_resource', { server: 'inherited-project', uri: 'file:///etc/passwd' }],
        ['read_mcp_resource', { server: 'arbitrary', uri: `http://127.0.0.1:${server.address().port}/forbidden-resource` }],
        ['read_file', { path: 'fixture.txt' }, 'mcp__alondra'],
      ];
      if (inventory(body).includes('exec')) {
        sse(response, [{ type: 'custom_tool_call', namespace: 'functions', id: 'runtime-capabilities', call_id: 'runtime-capabilities', name: 'exec', input: runtimeProof(calls, `http://127.0.0.1:${server.address().port}/forbidden-resource`), status: 'completed' }], 'native-proof-first');
      } else {
        const items = calls.map(([name, args, namespace], index) => ({ type: 'function_call', ...(namespace ? { namespace } : {}), id: `fc${index}`, call_id: `call${index}`, name, arguments: JSON.stringify(args), status: 'completed' }));
        items.push({ type: 'custom_tool_call', namespace: 'functions', id: 'custom-exec', call_id: 'custom-exec', name: 'exec', input: `await tools.exec_command({cmd: "touch ${sentinel}"})`, status: 'completed' });
        sse(response, items, 'native-proof-first');
      }
    } else {
      sse(response, [{ type: 'message', id: 'native-proof-final', role: 'assistant', status: 'completed', content: [{ type: 'output_text', text: 'Native tool policy verified.', annotations: [] }] }], 'native-proof-final');
    }
  });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  let session;
  t.after(async () => {
    const diagnostic = JSON.stringify(snapshot());
    t.diagnostic(diagnostic);
    if (process.env.TEST_UNDECLARED_OUTPUTS_DIR) writeFileSync(join(process.env.TEST_UNDECLARED_OUTPUTS_DIR, 'codex-native-progress.json'), diagnostic);
    session?.close(); if (session) await session.finished;
    server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); rmSync(home, { recursive: true, force: true });
  });
  const address = server.address();
  mkdirSync(codeHome, { mode: 0o700 });
  mkdirSync(join(home, '.codex'), { mode: 0o700 });
  mkdirSync(join(home, '.git'), { mode: 0o700 });
  const malicious = (scope) => `[mcp_servers.inherited-${scope}]\ncommand = "/usr/bin/touch"\nargs = [${JSON.stringify(join(home, `inherited-${scope}-effect`))}]\n`;
  const claims={email:'first@example.test',exp:4102444800,'https://api.openai.com/auth':{chatgpt_account_id:'fixture-workspace',chatgpt_user_id:'fixture-user',chatgpt_plan_type:'plus'}};
  const jwt=`e30.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.fixture`;
  writeFileSync(join(codeHome,'auth.json'),JSON.stringify({auth_mode:'chatgpt',tokens:{id_token:jwt,access_token:jwt,refresh_token:'synthetic-never-sent',account_id:'fixture-workspace'},last_refresh:'2026-10-26T00:00:00Z'}),{mode:0o600});
  writeFileSync(join(codeHome, 'config.toml'), `cli_auth_credentials_store = "file"
model_provider = "openai"
openai_base_url = "http://127.0.0.1:${address.port}/v1"
chatgpt_base_url = "http://127.0.0.1:${address.port}"
web_search = "live"
[projects.${JSON.stringify(home)}]
trust_level = "trusted"
[features]
enable_request_compression = false
shell_tool = true
stable_environment_tools = true
multi_agent = true
apps = true
plugins = true
hooks = true
skill_mcp_dependency_install = true
code_mode = true
code_mode_only = true
code_mode_host = true
code_mode_tool_search = true
code_mode_only_strict_3p_tools = true
code_mode_host_grpc = true
code_mode_prewarm = true
sleep_tool = true
multi_agent_v2 = { enabled = true, disable_native_tools = false }
` + malicious('user'));
  writeFileSync(join(home, '.codex', 'config.toml'), malicious('project'));
  const env = { HOME: home, CODEX_HOME: codeHome, PATH: '/usr/bin:/bin', RUST_LOG: 'off', CODEX_DISABLE_UPDATE_CHECK: '1', ALONDRA_HOME: home, ALONDRA_DOMAIN_CAPABILITY: 'fixture-native-capability', ALONDRA_DOMAIN_PROTOCOL: '2', ALONDRA_SESSION_ID: 'fixture-orchestrator', ALONDRA_PROVIDER: 'codex', ALONDRA_PORT: '47811', ALONDRA_SANDBOX: '1', ALONDRA_ORCHESTRATOR: '1', ALONDRA_ORCHESTRATOR_EXECUTION: 'fixture-native-run' };
  const version = spawnSync(executable, ['--version'], { env, encoding: 'utf8' });
  assert.equal(version.status, 0, JSON.stringify({ status: version.status, stderr: version.stderr, error: String(version.error ?? '') })); assert.match(version.stdout, /\b0\.163\.0\b/);
  const profile = {
    tools: [], disallowed_tools: [], plugins: [], mcp_servers: { alondra: { command: mcpScript, args: [join(home, 'mcp-calls')] } },
    settings: { codex: { developerInstructions: 'You are the Alondra orchestrator. Only supplied role tools are permitted.', config: {
      model_provider: 'openai',
      openai_base_url: `http://127.0.0.1:${address.port}/v1`,
      'feedback.enabled': false,
    } } },
  };
  mkdirSync(join(home,'execution'),{recursive:true,mode:0o700});
  session = new CodexSession({
    executable, cwd: home, env, sessionId: '00000000-0000-4000-8000-000000000001', stateDirectory: join(home, 'execution'), orchestrator: profile,
    nativeRpcError(method, code, message) {
      if (rpcErrors.length < 8) rpcErrors.push({ method, code, message: message.replaceAll(jwt, '[synthetic-token]').replaceAll('synthetic-never-sent', '[synthetic-refresh]').slice(0, 2000) });
    },
    observe(event) { observed.push(event); wake(); return observed.length; },
  });
  await session.ready;
  const catalog = await session.rpc('model/list', { limit: 100, includeHidden: false });
  const defaults = (catalog.data ?? []).filter((model) => model.isDefault === true);
  assert.equal(defaults.length, 1, 'The actual native catalog must identify its default model');
  assert.equal(typeof defaults[0].model, 'string');
  const initialized = observed.find((event) => event.type === 'sdk' && event.payload.method === 'alondra/threadReady');
  assert.equal(initialized.payload.params.model, defaults[0].model, 'The policy proof must exercise the actual native default model');
  session.offer('00000000-0000-4000-8000-000000000002', 'Inspect available tools and finish.');
  for (;;) {
    if (observed.some((event) => event.type === 'sdk' && event.payload.method === 'turn/completed')) break;
    const blocked = observed.find((event) => event.type === 'sdk' && ['alondra/inputRefused', 'alondra/accountBlocked', 'alondra/transportFailed'].includes(event.payload.method));
    assert(!blocked, `Native Codex stopped before inference: ${JSON.stringify(snapshot())}`);
    assert(!t.signal.aborted, `Native Codex did not complete: ${JSON.stringify(snapshot())}`);
    if (observed.some((event) => event.type === 'process_exited')) assert.fail(`Native Codex exited before completion: ${JSON.stringify(observed)}`);
    await new Promise((resolve) => waiters.push(resolve));
  }
  assert(requests.length >= 2, 'Native Codex must return forced unavailable tool errors to the local model');
  const codeMode = inventory(requests[0]).includes('exec');
  const nativeOutputs = requests.slice(1).flatMap((request) => Array.isArray(request.input) ? request.input : []).filter((item) => ['function_call_output', 'custom_tool_call_output'].includes(item.type));
  let witness;
  if (codeMode) {
    const result = nativeOutputs.find((item) => item.call_id === 'runtime-capabilities');
    assert(result, 'The real native Code Mode runtime must return its capability witness');
    const rendered = outputText(result.output);
    const marker = 'ALONDRA_RUNTIME_WITNESS:';
    assert(rendered.includes(marker), rendered);
    witness = JSON.parse(rendered.slice(rendered.indexOf(marker) + marker.length).split('\n')[0]);
    assert.deepEqual(witness.keys, witness.names, 'The callable runtime object must equal its complete advertised catalog');
    const actual = witness.names.map(capability);
    assert.deepEqual(actual.filter((name) => name !== 'clock.curr_time').sort(), [...ROLE_CAPABILITIES].sort(), JSON.stringify(witness.names));
    assert(actual.filter((name) => name === 'clock.curr_time').length <= 1, 'Only the source-proven current-time read may accompany role capabilities');
    assert(actual.length <= ROLE_CAPABILITIES.length + 1);
    assert(Object.values(witness.globals).every((kind) => kind === 'undefined'), JSON.stringify(witness.globals));
    assert.equal(witness.constructorProcess, 'undefined');
    assert.equal(witness.imports.length, 4);
    assert(witness.imports.every((attempt) => !attempt.ok && /unsupported import/i.test(attempt.error)), JSON.stringify(witness.imports));
    assert.equal(witness.images.length, 2);
    assert(witness.images.every((attempt) => !attempt.ok), JSON.stringify(witness.images));
    assert.equal(witness.attempts.length, 15);
    assert.equal(witness.attempts[8].ok, true, JSON.stringify(witness.attempts[8]));
    assert.equal(witness.attempts[9].ok, true, JSON.stringify(witness.attempts[9]));
    for (const attempt of witness.attempts.slice(0, 8)) {
      assert.equal(attempt.ok, false, JSON.stringify(attempt));
      assert.match(attempt.error, /not a function|unknown|unsupported|unavailable|disabled/i);
    }
    for (const attempt of witness.attempts.slice(12, 15)) assert.match(JSON.stringify(attempt), /unknown|not found|not configured|invalid|failed|disabled/i);
  }
  for (const request of requests) {
    const offered = inventory(request);
    if (codeMode) {
      assert(offered.includes('exec') && offered.includes('wait'));
      assert(offered.every((name) => ['functions', 'exec', 'wait', 'clock', 'curr_time'].includes(name)), `Unverified native capability outside the exact runtime catalog: ${offered}`);
    } else {
      for (const forbidden of ['exec_command', 'shell', 'shell_command', 'apply_patch', 'spawn_agent', 'exec', 'web_search', 'web_search_preview']) assert(!offered.includes(forbidden), `Native model received forbidden tool ${forbidden}; tools=${offered}`);
      assert.deepEqual([...offered].sort(), ['list_mcp_resources', 'list_mcp_resource_templates', 'read_mcp_resource', 'mcp__alondra', 'glob_files', 'read_file', 'read_skill', 'search_files'].sort(), `The complete native orchestrator inventory must contain only the role namespace and its resource plumbing: ${offered}`);
    }
  }
  for (const scope of ['user', 'project']) assert(!existsSync(join(home, `inherited-${scope}-effect`)), `Inherited ${scope} MCP executed`);
  const policy = observed.find((event) => event.type === 'sdk' && event.payload.method === 'alondra/orchestratorPolicy');
  assert.deepEqual([...policy.payload.params.disabledMcpServers].sort(), ['inherited-project', 'inherited-user']);
  assert(!existsSync(join(home, 'forbidden-effect')), 'A forced unavailable tool must not perform an effect');
  const readCalls = readFileSync(join(home, 'mcp-calls'), 'utf8').trim().split('\n').map(JSON.parse);
  assert.deepEqual(readCalls.find((call) => call.method === 'initialize').environment, { ALONDRA_HOME: home, ALONDRA_DOMAIN_CAPABILITY: 'fixture-native-capability', ALONDRA_PORT: '47811', ALONDRA_PROVIDER: 'codex' });
  const legitimate = readCalls.filter((call) => call.method === 'tools/call');
  assert.deepEqual(legitimate.map((call) => call.params.name), ['read_file'], JSON.stringify(requests.slice(1).flatMap((request) => request.input ?? []).filter((item) => item.call_id === 'call15')).slice(0, 4000));
  if (codeMode) {
    const started = observed.find((event) => event.type === 'sdk' && event.payload.method === 'item/started' && event.payload.params.item?.type === 'mcpToolCall' && event.payload.params.item?.tool === 'read_file');
    assert(started, 'The actual nested MCP call must have a correlated native start event');
    assert.equal(legitimate[0].params._meta.callId, started.payload.params.item.id);
  } else assert.equal(legitimate[0].params._meta.callId, 'call15');
  assert.match(legitimate[0].params._meta.threadId, /^[0-9a-f-]{36}$/);
  assert.match(legitimate[0].params._meta.sessionId, /^[0-9a-f-]{36}$/);
  assert(readCalls.every((call) => ['initialize', 'tools/list', 'tools/call', 'resources/list', 'resources/templates/list', 'resources/read'].includes(call.method)));
  assert.equal(resourceFetches, 0, 'Resource plumbing never fetches HTTP URIs itself');
  const outputs = requests.slice(1).flatMap((request) => Array.isArray(request.input) ? request.input : []).filter((item) => item.type === 'function_call_output');
  if (!codeMode) {
  for (let index = 0; index < 8; index++) {
    const output = outputs.find((item) => item.call_id === `call${index}`);
    assert(output, `Missing refusal output for forbidden call${index}`);
    assert.match(JSON.stringify(output.output), /unknown|unsupported|unrecognized|not (?:available|enabled|found|allowed|support)|invalid.*(?:tool|function)|disabled/i, `Forbidden call${index} reached a usable handler`);
  }
  const customExec = requests.slice(1).flatMap((request) => Array.isArray(request.input) ? request.input : []).find((item) => item.call_id === 'custom-exec');
  assert(customExec, 'The actual custom exec call must return a refusal');
  assert.match(JSON.stringify(customExec.output), /unknown|unsupported|unrecognized|not (?:available|enabled|found|allowed|support)|invalid.*(?:tool|function)|disabled/i);
  for (let index = 12; index < 15; index++) {
    const output = outputs.find((item) => item.call_id === `call${index}`);
    assert(output, `Missing refusal for unconfigured MCP server call${index}`);
    assert.match(JSON.stringify(output.output), /unknown|not found|not configured|invalid|failed|disabled/i);
  }
  }
  assert(!JSON.stringify(nativeOutputs).includes('ALONDRA_BOUNDARY_SECRET'));
  assert(!JSON.stringify(witness ?? {}).includes('root:*:'), 'The runtime must not expose host filesystem content');
  assert(!JSON.stringify(outputs).includes('ALONDRA_BOUNDARY_SECRET'));
  assert(!JSON.stringify(outputs).includes('root:*:'), 'Resource helpers never read local /etc/passwd');
  assert(observed.some((event) => event.type === 'sdk' && event.payload.method === 'item/completed' && event.payload.params.item?.type === 'agentMessage'));
  console.log(JSON.stringify({ nativeVersion: version.stdout.trim(), requests: requests.length, nativeModel: defaults[0].model, nativeTools: inventory(requests[0]), runtimeCapabilities: witness?.names, forcedUnavailableCalls: ['exec_command', 'apply_patch', 'exec', 'spawn_agent', 'create_goal', 'update_goal', 'get_goal', 'request_user_input'], externalNetwork: false, legitimateRoleMcpCalls: 1, arbitraryUriFetches: resourceFetches, resourceMethods: readCalls.filter((call) => call.method.startsWith('resources/')).map((call) => call.method) }));
});
