import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { mkdtempSync, mkdirSync, writeFileSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir, userInfo } from 'node:os';
import { join, resolve } from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { CodexSession } from './codex-session.js';

function sse(response, items, id) {
  response.writeHead(200, { 'content-type': 'text/event-stream', 'cache-control': 'no-cache', connection: 'keep-alive' });
  const send = (type, value) => response.write(`event: ${type}\ndata: ${JSON.stringify({ type, ...value })}\n\n`);
  send('response.created', { response: { id, object: 'response', status: 'in_progress', output: [] } });
  for (const [index, item] of items.entries()) {
    send('response.output_item.added', { output_index: index, item });
    send('response.output_item.done', { output_index: index, item });
  }
  send('response.completed', { response: { id, object: 'response', status: 'completed', output: items, usage: { input_tokens: 16, output_tokens: 4, total_tokens: 20, input_tokens_details: { cached_tokens: 0 }, output_tokens_details: { reasoning_tokens: 0 } } } });
  response.end();
}
function final(text, id) {
  return { type: 'message', id, role: 'assistant', phase: 'final_answer', status: 'completed', content: [{ type: 'output_text', text, annotations: [] }] };
}
function outputTexts(output) {
  if (typeof output === 'string') return [output];
  assert(Array.isArray(output), 'A native tool output must be text or content items');
  assert.equal(output[0]?.type, 'input_text', 'The first native content item holds its trusted status header');
  return output.filter((item) => item.type === 'input_text' && typeof item.text === 'string').map((item) => item.text);
}

for (const coldResume of [false, true]) test(`an actual Codex script paused after its turn is not work and advances only in a later turn (${coldResume ? 'cold resume' : 'fresh'})`, { timeout: 60000 }, async (t) => {
  const native = process.env.CODEX_NATIVE_EXECUTABLE;
  assert(native, 'This opt-in target requires an explicit CODEX_NATIVE_EXECUTABLE');
  const home = mkdtempSync(join(tmpdir(), 'alondra-codex-native-cells-'));
  const codeHome = join(home, 'codex-home');
  mkdirSync(codeHome, { mode: 0o700 });
  mkdirSync(join(home, 'execution'), { mode: 0o700 });
  mkdirSync(join(home, '.git'), { mode: 0o700 });
  const profile = join(home, 'native.sb');
  const realHome = JSON.stringify(userInfo().homedir);
  writeFileSync(profile, `(version 1)(allow default)(deny file-read* (subpath ${realHome}))(deny file-write* (subpath ${realHome}))(deny mach-lookup (global-name "com.apple.securityd"))(deny mach-lookup (global-name "com.apple.securityd.xpc"))(deny network*)(allow network-inbound (local ip "localhost:*"))(allow network-outbound (remote ip "localhost:*"))`);
  const quote = (value) => "'" + value.replaceAll("'", "'\\''") + "'";
  const executable = join(home, 'native-codex');
  writeFileSync(executable, `#!/bin/sh\nexec /usr/bin/sandbox-exec -f ${quote(profile)} ${quote(native)} "$@"\n`, { mode: 0o700 });
  const mcp = join(home, 'cell-mcp');
  writeFileSync(mcp, readFileSync(resolve('agent-host/codex-native-cell-mcp.py')), { mode: 0o700 });
  const observed = []; const requests = []; const waiters = []; const rpcErrors = []; const waitEvidence = []; const waitSchemas = [];
  let warming = coldResume; let warmupModelRequests = 0;
  const event = (method) => observed.filter((entry) => entry.type === 'sdk' && entry.payload.method === method);
  const snapshot = () => ({ modelRequests: requests.length, warmupModelRequests, waitEvidence, waitSchemas, rpcErrors, firstEntered, firstReleased, secondEntered, methods: observed.slice(-32).map((entry) => entry.type === 'sdk' ? { method: entry.payload.method } : { type: entry.type }) });
  const wake = () => { for (const waiter of waiters.splice(0)) waiter(); };
  t.signal.addEventListener('abort', wake, { once: true });
  let firstEntered = false; let firstReleased = false; let secondEntered = false;
  let firstResponse; let cellId; let session; let fixtureFailure;
  const until = async (predicate) => {
    while (!predicate()) {
      assert(!fixtureFailure, fixtureFailure?.stack);
      assert(!t.signal.aborted, JSON.stringify(snapshot()));
      const blocked = observed.find((entry) => entry.type === 'sdk' && ['alondra/transportFailed', 'alondra/inputRefused', 'alondra/accountBlocked'].includes(entry.payload.method));
      assert(!blocked, JSON.stringify(snapshot()));
      assert(!observed.some((entry) => entry.type === 'process_exited'), JSON.stringify(snapshot()));
      await new Promise((resolve) => waiters.push(resolve));
    }
  };
  const code = `// @exec: {"yield_time_ms": 1, "max_output_tokens": 2000}
const matches = ALL_TOOLS.filter(tool => ['mcp__alondra__barrier','mcp__alondra_barrier','mcp__alondra.barrier'].includes(tool.name));
if (matches.length !== 1) throw new Error('Expected exactly one fixture barrier');
const first = await tools[matches[0].name]({phase:'first'});
const second = await tools[matches[0].name]({phase:'second'});
text('ALONDRA_CELL_DONE:'+JSON.stringify({first,second}));`;
  const handle = async (request, response) => {
    const chunks = []; for await (const chunk of request) chunks.push(chunk);
    const path = request.url.split('?')[0];
    if (path === '/mcp/first') {
      assert.equal(firstEntered, false); firstEntered = true; firstResponse = response; wake(); return;
    }
    if (path === '/mcp/second') {
      assert(firstReleased);
      assert.equal(event('turn/completed').length, 1, 'The later callback runs after the first root turn');
      assert(event('turn/started').length >= 2, 'A new positive native root turn services the queued callback');
      secondEntered = true; response.writeHead(200, { 'content-type': 'application/json' }); response.end('{"phase":"second","delivered":true}'); wake(); return;
    }
    if (request.method === 'GET') {
      const account = path.includes('accounts/check');
      response.writeHead(account ? 200 : 404, { 'content-type': 'application/json' });
      response.end(JSON.stringify(account ? { accounts: [{ id: 'fixture-workspace', plan_type: 'plus', workspace_backend_origin: 'https://chatgpt.com', account_routing_override: 'NO_CONSTRAINT' }], account_ordering: ['fixture-workspace'], default_account_id: 'fixture-workspace' } : { error: 'No remote catalog in this fixture' })); return;
    }
    if (path !== '/v1/responses') { response.writeHead(204); response.end(); return; }
    assert(!request.headers['content-encoding'], 'The stable fixture compression override must be effective');
    const body = JSON.parse(Buffer.concat(chunks)); requests.push(body); wake();
    if (warming) { warmupModelRequests++; sse(response, [final('Native warmup complete without tools.', 'fixture-warmup-final')], 'fixture-response-warmup'); return; }
    if (requests.length === 3) {
      const visit = (value) => { if (Array.isArray(value)) { for (const child of value) visit(child); } else if (value && typeof value === 'object') { if (value.name === 'wait') waitSchemas.push(JSON.stringify(value).slice(0, 4000)); for (const child of Object.values(value)) visit(child); } };
      visit(body.tools);
    }
    switch (requests.length) {
      case 1:
        sse(response, [{ type: 'custom_tool_call', namespace: 'functions', name: 'exec', id: 'fixture-exec', call_id: 'fixture-exec', input: code, status: 'completed' }], 'fixture-response-first'); return;
      case 2: {
        const execOutputs = (body.input ?? []).filter((item) => item.type === 'custom_tool_call_output' && item.call_id === 'fixture-exec');
        assert.equal(execOutputs.length, 1, 'Read the exact exec result, never an example from tool instructions');
        const header = outputTexts(execOutputs[0].output)[0];
        const running = /^Script running with cell ID ([0-9]+)\n/.exec(header);
        assert(running, `The real native exec must yield its numeric cell ID: ${header.slice(0, 200)}`);
        cellId = running[1];
        await until(() => firstEntered);
        sse(response, [final('The root turn ends while the yielded cell stays alive.', 'fixture-first-final')], 'fixture-response-second'); return;
      }
      case 3:
        assert(firstReleased); assert(cellId);
        sse(response, [{ type: 'function_call', namespace: 'functions', name: 'wait', id: 'fixture-wait', call_id: 'fixture-wait', arguments: JSON.stringify({ cell_id: cellId, yield_time_ms: 10000, max_tokens: 2000 }), status: 'completed' }], 'fixture-response-third'); return;
      case 4: {
        const waitOutput = (body.input ?? []).find((item) => item.call_id === 'fixture-wait' && item.type === 'function_call_output');
        assert(waitOutput, 'The exact native wait must return its correlated function output');
        const chunks = outputTexts(waitOutput.output);
        const output = chunks.join('\n');
        waitEvidence.push({ type: waitOutput.type, callId: waitOutput.call_id, output: output.slice(0, 4000) });
        assert(/^Script completed\n/.test(chunks[0]) && output.includes('ALONDRA_CELL_DONE:'), `The exact native wait must positively finish the original cell: ${JSON.stringify(waitEvidence.at(-1))}`);
        sse(response, [final('The original native cell finished through its queued callback.', 'fixture-second-final')], 'fixture-response-fourth'); return;
      }
      default: assert.fail(`Unexpected native model request ${requests.length}`);
    }
  };
  const server = createServer((request, response) => { void handle(request, response).catch((error) => { fixtureFailure = error; wake(); if (!response.headersSent) response.writeHead(500); response.end('Synthetic fixture assertion failed'); }); });
  await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
  t.after(async () => {
    const diagnostic = JSON.stringify(snapshot()); t.diagnostic(diagnostic);
    if (process.env.TEST_UNDECLARED_OUTPUTS_DIR) writeFileSync(join(process.env.TEST_UNDECLARED_OUTPUTS_DIR, `codex-native-cells-${coldResume ? 'resumed' : 'fresh'}-progress.json`), diagnostic);
    firstResponse?.destroy(); session?.close(); if (session) await session.finished;
    server.closeAllConnections(); await new Promise((resolve) => server.close(resolve)); rmSync(home, { recursive: true, force: true });
  });
  const endpoint = `http://127.0.0.1:${server.address().port}`;
  const claims = { email: 'first@example.test', exp: 4102444800, 'https://api.openai.com/auth': { chatgpt_account_id: 'fixture-workspace', chatgpt_user_id: 'fixture-user', chatgpt_plan_type: 'plus' } };
  const jwt = `e30.${Buffer.from(JSON.stringify(claims)).toString('base64url')}.fixture`;
  writeFileSync(join(codeHome, 'auth.json'), JSON.stringify({ auth_mode: 'chatgpt', tokens: { id_token: jwt, access_token: jwt, refresh_token: 'synthetic-never-sent', account_id: 'fixture-workspace' }, last_refresh: '2026-10-26T00:00:00Z' }), { mode: 0o600 });
  writeFileSync(join(codeHome, 'config.toml'), `cli_auth_credentials_store="file"\nmodel_provider="openai"\nopenai_base_url="${endpoint}/v1"\nchatgpt_base_url="${endpoint}"\n[features]\nenable_request_compression=false\napps=false\nplugins=false\nhooks=false\ncode_mode=true\n`);
  const env = { HOME: home, CODEX_HOME: codeHome, PATH: '/usr/bin:/bin', RUST_LOG: 'off', ALONDRA_HOME: home, ALONDRA_DOMAIN_CAPABILITY: 'fixture-cell-capability', ALONDRA_DOMAIN_PROTOCOL: '2', ALONDRA_SESSION_ID: 'fixture-cell-task', ALONDRA_PROVIDER: 'codex', ALONDRA_PORT: '47811', ALONDRA_SANDBOX: '1' };
  const version = spawnSync(executable, ['--version'], { env, encoding: 'utf8' });
  assert.equal(version.status, 0); assert.match(version.stdout, /\b0\.163\.0\b/);
  const configuration = { executable, cwd: home, env, sessionId: '00000000-0000-4000-8000-000000000041', processGeneration: 'fixture-native-cell-generation', stateDirectory: join(home, 'execution'), worker: { plugins: [], settings: { codex: { developerInstructions: 'Run the deterministic native-cell fixture.', mcp_servers: { alondra: { command: mcp, args: [endpoint] } }, config: { 'features.code_mode': true, 'features.enable_request_compression': false, 'features.apps': false, 'features.plugins': false, 'features.hooks': false } } } }, nativeRpcError(method, code, message) { if (rpcErrors.length < 8) rpcErrors.push({ method, code, message: message.replaceAll(jwt, '[synthetic-token]').slice(0, 1000) }); }, observe(value) { observed.push(value); wake(); return observed.length; } };
  if (coldResume) {
    session = new CodexSession({ ...configuration, processGeneration: 'fixture-native-cell-prewarm' });
    await session.ready;
    const originalThread = event('alondra/threadReady')[0].payload.params.threadId;
    assert.equal(requests.length, 0, 'No inference before the controlled warmup input');
    session.offer('00000000-0000-4000-8000-000000000044', 'Acknowledge this warmup without calling tools.');
    await until(() => event('turn/completed').length === 1);
    assert.equal(warmupModelRequests, 1); assert.equal(firstEntered, false);
    const retained = await session.rpc('thread/read', { threadId: originalThread, includeTurns: false });
    assert.equal(typeof retained.thread.path, 'string', 'A completed native warmup materializes its canonical rollout');
    session.close(); await session.finished; observed.length = 0; requests.length = 0; warming = false;
    session = new CodexSession({ ...configuration, resume: true });
    await session.ready;
    assert.equal(event('alondra/threadReady')[0].payload.params.threadId, originalThread, 'Cold resume retains the exact native conversation');
  } else { session = new CodexSession(configuration); await session.ready; }
  session.offer('00000000-0000-4000-8000-000000000042', 'Start the fixture cell and end this root turn while it stays alive.');
  await until(() => event('turn/completed').length === 1 && firstEntered);
  assert(cellId);
  const idle = event('thread/status/changed').at(-1);
  assert.equal(idle.payload.params.status.type, 'idle', 'Codex itself counts the thread idle while the script is paused');
  assert.equal(secondEntered, false);
  firstReleased = true; firstResponse.writeHead(200, { 'content-type': 'application/json' }); firstResponse.end('{"phase":"first","released":true}');
  await until(() => observed.some((entry) => entry.type === 'sdk' && entry.payload.method === 'item/completed' && entry.payload.params?.item?.type === 'mcpToolCall' && entry.payload.params.item.tool === 'barrier' && entry.payload.params.item.status === 'completed'));
  assert.equal(secondEntered, false, 'The paused script advances only in a later turn');
  session.offer('00000000-0000-4000-8000-000000000043', `Continue this same native session and wait for the outstanding cell ${cellId}.`);
  await until(() => event('turn/completed').length === 2);
  assert(!fixtureFailure, fixtureFailure?.stack);
  assert(secondEntered); assert.equal(requests.length, 4);
  assert.deepEqual(observed.filter((entry) => entry.type === 'sdk' && String(entry.payload.method).startsWith('alondra/codeCell')), []);
  assert.equal(observed.some((entry) => entry.type === 'process_exited'), false);
});
