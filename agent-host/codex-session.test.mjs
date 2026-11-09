import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import test from 'node:test';
import { createHash } from 'node:crypto';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { CodexSession, codexPolicy, alondraMcpEnvironment } from './codex-session.js';
import { InputNotOffered } from './host-session.js';
import { failedAdoptedHost } from './codex-host-failure.mjs';

function fixture(t, options = {}) {
  const home = mkdtempSync(join(tmpdir(), 'alondra-codex-'));
  const transcript = join(home, 'transcript');
  const accountFile=join(home,'native-account-report.json');
  if(options.account)writeFileSync(accountFile,JSON.stringify(options.account));
  const events = []; const waiting = [];
  const config = {
    executable: resolve('agent-host/fake-codex.py'), cwd: home,
    env: { HOME: home, PATH: '/usr/bin:/bin', ALONDRA_HOME: home, ALONDRA_DOMAIN_CAPABILITY: 'fixture-capability', ALONDRA_DOMAIN_PROTOCOL: '2', ALONDRA_SESSION_ID: 'fixture-node', ALONDRA_PROVIDER: 'codex', ...(options.config?.orchestrator ? { ALONDRA_ORCHESTRATOR: '1', ALONDRA_ORCHESTRATOR_EXECUTION: 'fixture-run' } : {}), ALONDRA_FAKE_ACCOUNT_FILE: accountFile, ALONDRA_FAKE_TRANSCRIPT: transcript, ALONDRA_FAKE_ARGV: join(home, 'argv'), ...(options.env ?? {}) },
    sessionId: '00000000-0000-4000-8000-000000000002',
    stateDirectory: join(home, 'execution'),
    observe(observation) {
      events.push(observation);
      options.onObserve?.(observation);
      for (const waiter of waiting.splice(0)) waiter();
      return events.length;
    },
    ...options.config,
  };
  mkdirSync(config.stateDirectory,{recursive:true,mode:0o700});
  const session = new CodexSession(config);
  const childClosed = new Promise((resolve) => session.child.once('close', resolve));
  t.after(async () => {
    try {
      session.close();
      if (options.nativeFailure) {
        if (session.child.exitCode === null && session.child.signalCode === null) session.child.kill('SIGKILL');
        await childClosed;
      } else await session.finished;
    } finally { rmSync(home, { recursive: true, force: true }); }
  });
  const next = async (predicate) => {
    for (;;) {
      const found = events.find(predicate);
      if (found) return found;
      await new Promise((resolve) => waiting.push(resolve));
    }
  };
  return { session, home, config, accountFile, events, next, messages: () => readFileSync(transcript, 'utf8').trim().split('\n').map(JSON.parse) };
}
const method = (name) => (event) => event.type === 'sdk' && event.payload.method === name;

test('an adopted persistent Codex host ends its native command group and retains exit evidence', { timeout: 15000 }, failedAdoptedHost);

test('a normally force-stopped adopted Codex host ends without its owner acknowledging exit', { timeout: 15000 }, (t) => failedAdoptedHost(t, 'codex.cjs', false, true));

test('a normally ended adopted Codex host keeps its owner lane through terminal ACK', { timeout: 15000 }, (t) => failedAdoptedHost(t, 'codex.cjs', false, true, true));

test('advancing terminal ACKs renew shutdown inactivity while duplicates do not', { timeout: 15000 }, (t) => failedAdoptedHost(t, 'codex-ack-progress-fixture.mjs', false, true, true, true));

test('failed-session cleanup keeps ownership through blocked output and a journal-close error', { timeout: 15000 }, (t) => failedAdoptedHost(t, 'codex-failed-cleanup-fixture.mjs', true));

test('native evidence already written before exit drains before the exit observation', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { nativeFailure: true, env: { ALONDRA_FAKE_MODE: 'hold', ALONDRA_FAKE_FINAL_TEXT: 'Final native evidence' } });
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Exit after writing final evidence');
  await f.next(method('alondra/inputAccepted'));
  f.session.child.stdout.pause();
  f.session.child.once('exit', () => f.session.child.stdout.resume());
  writeFileSync(f.accountFile, JSON.stringify({ exitMcpAfterFinal: true }));
  const control = f.session.mcpServerStatus();
  const rejected = assert.rejects(control, /Codex process ended/);
  await f.session.finished;
  await rejected;
  const final = f.events.findIndex((event) => method('item/completed')(event) && event.payload.params.item.text === 'Final native evidence');
  assert(final >= 0, 'the final native item is retained');
  assert(final < f.events.findIndex((event) => event.type === 'process_exited'));
});

test('native turn acceptance survives exit while its response is still unread', { timeout: 15000 }, async (t) => {
  const f = fixture(t, {
    nativeFailure: true,
    env: { ALONDRA_FAKE_MODE: 'complete_then_exit', ALONDRA_FAKE_FINAL_TEXT: 'Final accepted turn' },
    onObserve(observation) { if (method('alondra/inputStarting')(observation)) f.session.child.stdout.pause(); },
  });
  await f.next(method('alondra/threadReady'));
  f.session.child.once('exit', () => f.session.child.stdout.resume());
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Read acceptance after native exit');
  await f.session.finished;
  const exit = f.events.findIndex((event) => event.type === 'process_exited');
  const accepted = f.events.findIndex(method('alondra/inputAccepted'));
  const final = f.events.findIndex((event) => method('item/completed')(event) && event.payload.params.item.text === 'Final accepted turn');
  assert(accepted >= 0 && accepted < exit, 'native acceptance precedes exit evidence');
  assert(final >= 0 && final < exit, 'the accepted turn retains its final native item');
});

test('native stdout EOF ends the session even when the app-server process stays alive', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { nativeFailure: true, env: { ALONDRA_FAKE_MODE: 'close_stdout' } });
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Close only the output pipe');
  await f.next(method('alondra/inputAccepted'));
  await f.session.finished;
  assert.equal(f.session.failure, 'native_output_closed');
  assert.equal(f.events.filter((event) => event.type === 'process_exited').length, 1);
  assert.throws(() => f.session.offer('00000000-0000-4000-8000-000000000004', 'Must not disappear'), InputNotOffered);
});

test('stdout EOF rejects a pending control and kills a native process ignoring SIGTERM', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { nativeFailure: true });
  await f.next(method('alondra/threadReady'));
  writeFileSync(f.accountFile, JSON.stringify({ closeMcpStdout: true }));
  await assert.rejects(f.session.mcpServerStatus(), /Codex process ended/);
  await f.session.finished;
  assert.equal(f.session.failure, 'native_output_closed');
  assert.equal(f.events.find((event) => event.type === 'process_exited').signal, 'SIGKILL');
});

test('a missing executable finishes the session after an asynchronous spawn failure', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { nativeFailure: true, config: { executable: resolve('agent-host/missing-codex-fixture') } });
  await f.session.finished;
  assert.equal(f.session.failure, 'native_spawn_failed');
  assert.equal(f.events.filter((event) => event.type === 'process_exited').length, 1);
});

test('native exit releases host pipes inherited by a surviving descendant', { timeout: 15000 }, async (t) => {
  const home = mkdtempSync(join(tmpdir(), 'alondra-codex-exit-'));
  const host = spawn(process.execPath, [resolve('agent-host/codex-exit-fixture.mjs'), home, resolve('agent-host/fake-codex.py')], { stdio: 'ignore' });
  const exited = once(host, 'exit');
  t.after(async () => {
    try {
      try { process.kill(JSON.parse(readFileSync(join(home, 'descendant'), 'utf8')), 'SIGKILL'); } catch (error) { if (!['ENOENT', 'ESRCH'].includes(error.code)) throw error; }
      if (host.exitCode === null && host.signalCode === null) { host.kill('SIGKILL'); await exited; }
    } finally { rmSync(home, { recursive: true, force: true }); }
  });
  const [code, signal] = await exited;
  assert.equal(code, 0);
  assert.equal(signal, null);
  assert.equal(typeof JSON.parse(readFileSync(join(home, 'descendant'), 'utf8')), 'number');
});

test('raw native final item is authoritative and input acceptance precedes early notifications', { timeout: 15000 }, async (t) => {
  const f = fixture(t);
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Hello');
  await f.next(method('turn/completed'));
  const accepted = f.events.findIndex(method('alondra/inputAccepted'));
  assert(accepted < f.events.findIndex(method('turn/started')));
  const final = f.events.find(method('item/completed'));
  assert.equal(final.payload.params.item.text, '');
  assert.equal(final.payload.type, undefined);
  const messages = f.messages();
  assert.equal(messages.filter((m) => m.method === 'initialize').length, 1);
  assert.equal(messages.filter((m) => m.method === 'turn/start').length, 1);
  const mapping = JSON.parse(readFileSync(join(f.home, 'native-threads', '00000000-0000-4000-8000-000000000002')));
  assert.equal(mapping.threadId, '00000000-0000-4000-8000-000000000001');
});

test('same native request is shown once and only exact epoch/item/request can answer it', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { env: { ALONDRA_FAKE_MODE: 'question' } });
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Ask');
  const asked = await f.next((event) => event.type === 'decision_requested');
  const epoch = f.events.indexOf(asked) + 1;
  const request = { request_id: asked.request_id, epoch, kind: asked.kind, tool_use_id: asked.tool_use_id, asked: asked.payload };
  const answer = { behavior: 'allow', updated_input: { answers: { language: { answers: ['English'] } } } };
  assert.equal(f.session.respond({ ...request, epoch: epoch + 1 }, answer), false);
  assert.equal(f.session.respond({ ...request, request_id: 'string:77' }, answer), false);
  assert.equal(f.session.respond(request, answer), true);
  assert.equal(f.session.respond(request, answer), false);
  await f.next(method('turn/completed'));
  assert.equal(f.events.filter((event) => event.type === 'decision_requested').length, 1);
  const response = f.messages().find((message) => message.id === 77 && message.result);
  assert.deepEqual(response.result, { answers: { language: { answers: ['English'] } } });
});

test('orchestrator policy removes inherited MCP and all mutating native tool families', { timeout: 15000 }, async (t) => {
  const profile = { model: 'fixture-model', tools: [], disallowed_tools: [], plugins: [], mcp_servers: { alondra: { command: '/safe/alondra', args: ['orchestrator-mcp'] } }, settings: { codex: { developerInstructions: 'Persistent role instructions', config: { 'features.image_generation': true, 'features.imagegenext': true, 'features.code_mode': true, 'features.code_mode_only': true, 'features.code_mode_host': true, 'features.code_mode_tool_search': true, 'features.code_mode_only_strict_3p_tools': true, 'features.code_mode_host_grpc': true, 'features.code_mode_prewarm': true, 'features.sleep_tool': true, 'features.multi_agent_v2': { enabled: true, disable_native_tools: false } } } } };
  const f = fixture(t, { config: { orchestrator: profile } });
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Plan work');
  await f.session.finished;
  const messages = f.messages();
  const start = messages.find((message) => message.method === 'thread/start').params;
  assert.equal(start.experimentalRawEvents, undefined);
  assert.equal(start.config['mcp_servers.inherited-danger.enabled'], false);
  assert.equal(start.config['mcp_servers.alondra'].required, true);
  for (const feature of ['shell_tool', 'stable_environment_tools', 'multi_agent', 'apps', 'plugins', 'hooks', 'skill_mcp_dependency_install', 'image_generation', 'imagegenext', 'code_mode', 'code_mode_only', 'code_mode_host', 'code_mode_tool_search', 'code_mode_only_strict_3p_tools', 'code_mode_host_grpc', 'code_mode_prewarm', 'sleep_tool']) assert.equal(start.config[`features.${feature}`], false);
  assert.equal(start.config['agents.enabled'], false);
  assert.deepEqual(start.config['features.multi_agent_v2'], { enabled: false, disable_native_tools: true });
  assert.equal(start.config.web_search, 'disabled');
  assert.deepEqual(start.environments, []);
  assert.equal(start.developerInstructions, 'Persistent role instructions');
  const turn = messages.find((message) => message.method === 'turn/start').params;
  assert.deepEqual(turn.environments, []);
  assert.equal(turn.approvalPolicy, 'never');
  assert.deepEqual(turn.sandboxPolicy, { type: 'readOnly' });
});

test('same account model and effort apply on next turn while role instructions stay thread scoped', async () => {
  const config = { cwd: '/workspace', worker: { model: 'first', effort: 'high', settings: { codex: { developerInstructions: 'Keep me' } } } };
  const first = codexPolicy(config, 'plan', 'first');
  const next = codexPolicy(config, 'default', 'second');
  assert.equal(first.model, 'first'); assert.equal(next.model, 'second');
  assert.equal(next.effort, 'high'); assert.equal(next.developerInstructions, undefined);
  assert.equal(first.collaborationMode.mode, 'plan'); assert.equal(next.collaborationMode.mode, 'default');
});

test('a native plan approves into the next default turn only after its exact card answer', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { env: { ALONDRA_FAKE_MODE: 'plan' }, config: { worker: { model: 'fixture-model', plugins: [], settings: { codex: { mcp_servers: { alondra: { command: '/safe/alondra', args: ['mcp-node'] } } } }, disallowed_tools: [] } } });
  await f.session.setPermissionMode('plan');
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Plan this change');
  const plan = await f.next((event) => event.type === 'decision_requested' && event.kind === 'plan');
  assert.equal(f.messages().filter((message) => message.method === 'turn/start').length, 1);
  const request = { request_id: plan.request_id, epoch: f.events.indexOf(plan) + 1, kind: 'plan', tool_use_id: plan.tool_use_id, asked: plan.payload };
  assert.equal(f.session.respond({ ...request, epoch: request.epoch + 1 }, { behavior: 'allow', updated_input: {} }), false);
  assert.equal(f.session.respond(request, { behavior: 'allow', updated_input: {} }), true);
  assert.equal(f.session.respond(request, { behavior: 'allow', updated_input: {} }), false);
  const continued = await f.next(method('alondra/planContinuationAccepted'));
  assert.equal(continued.payload.params.epoch, request.epoch);
  const turns = f.messages().filter((message) => message.method === 'turn/start');
  assert.equal(turns.length, 2);
  assert.equal(turns[0].params.collaborationMode.mode, 'plan');
  assert.equal(turns[1].params.collaborationMode.mode, 'default');
  assert.equal(f.events.filter((event) => event.type === 'decision_requested' && event.kind === 'plan').length, 1);
});

for (const answerNewestFirst of [false, true]) {
  test(`a superseded native plan closes without another revision turn (newest answered first: ${answerNewestFirst})`, { timeout: 15000 }, async (t) => {
    const f = fixture(t, { env: { ALONDRA_FAKE_MODE: 'replace_plan' } });
    f.session.offer('00000000-0000-4000-8000-000000000003', 'Plan the change');
    const first = await f.next((event) => event.type === 'decision_requested' && event.kind === 'plan');
    await f.next(method('turn/completed'));
    f.session.offer('00000000-0000-4000-8000-000000000004', 'Revise the plan');
    const newest = await f.next((event) => event.type === 'decision_requested' && event.kind === 'plan' && event !== first);
    const request = (event) => ({ request_id: event.request_id, epoch: f.events.indexOf(event) + 1, kind: event.kind, tool_use_id: event.tool_use_id, asked: event.payload });
    const allow = () => assert.equal(f.session.respond(request(newest), { behavior: 'allow', updated_input: {} }), true);
    if (answerNewestFirst) {
      allow();
      await f.next((event) => method('alondra/planContinuationAccepted')(event) && event.payload.params.epoch === request(newest).epoch);
    }
    assert.equal(f.session.respond(request(first), { behavior: 'superseded' }), true);
    const cancelled = await f.next((event) => event.type === 'decision_settled' && event.epoch === request(first).epoch);
    assert.equal(cancelled.outcome, 'cancelled');
    assert.equal(f.session.respond(request(first), { behavior: 'superseded' }), false);
    if (!answerNewestFirst) allow();
    await f.next((event) => method('alondra/planContinuationAccepted')(event) && event.payload.params.epoch === request(newest).epoch);
    const turns = f.messages().filter((message) => message.method === 'turn/start');
    assert.equal(turns.length, 3, 'two planning turns and one approved implementation');
    assert.equal(turns.filter((message) => message.params.input[0].text.startsWith('Revise the plan using this user feedback:')).length, 0);
    assert.equal(f.events.filter((event) => event.type === 'decision_requested' && event.kind === 'plan').length, 2);
    assert.equal(f.events.filter((event) => event.type === 'decision_settled' && event.epoch === request(first).epoch).length, 1);
  });
}

test('denying a newer native plan by policy preserves the users feedback on the older plan', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { env: { ALONDRA_FAKE_MODE: 'replace_plan' } });
  f.session.offer('00000000-0000-4000-8000-000000000003', 'Plan the change');
  const first = await f.next((event) => event.type === 'decision_requested' && event.kind === 'plan');
  await f.next(method('turn/completed'));
  f.session.offer('00000000-0000-4000-8000-000000000004', 'Another plan while the task waits');
  const newer = await f.next((event) => event.type === 'decision_requested' && event.kind === 'plan' && event !== first);
  const request = (event) => ({ request_id: event.request_id, epoch: f.events.indexOf(event) + 1, kind: event.kind, tool_use_id: event.tool_use_id, asked: event.payload });
  assert.equal(f.session.respond(request(newer), { behavior: 'deny', message: 'The task waits for another task before its plan can be approved' }), true);
  await f.next((event) => method('alondra/planContinuationAccepted')(event) && event.payload.params.epoch === request(newer).epoch);
  const feedback = 'The user requests changes to the plan: keep the existing parser';
  assert.equal(f.session.respond(request(first), { behavior: 'deny', message: feedback }), true);
  const continuation = await f.next((event) => method('alondra/planContinuationAccepted')(event) && event.payload.params.epoch === request(first).epoch);
  const turns = f.messages().filter((message) => message.method === 'turn/start');
  assert.equal(turns.length, 4);
  assert.equal(turns.filter((message) => message.params.input[0].text === `Revise the plan using this user feedback: ${feedback}`).length, 1);
  assert.equal(continuation.payload.params.requestId, first.request_id);
  assert.equal(f.session.respond(request(first), { behavior: 'deny', message: feedback }), false);
});

for (const accountId of ['default', 'work']) {
  test(`only additional account ${accountId} forces file credential storage`, { timeout: 15000 }, async (t) => {
    const f = fixture(t, { config: { accountId, hostRelease: 'fixture-release' } });
    await f.next(method('alondra/threadReady'));
    assert.deepEqual(JSON.parse(readFileSync(join(f.home, 'argv'), 'utf8')), accountId === 'default' ? ['app-server', '--listen', 'stdio://'] : ['-c', 'cli_auth_credentials_store="file"', 'app-server', '--listen', 'stdio://']);
    assert.equal(f.messages().find((message) => message.method === 'initialize').params.clientInfo.version, 'fixture-release');
  });
}

test('role MCP environment is explicit and incomplete or confused role contexts fail before spawn', () => {
  const config = { worker: { settings: {} }, env: { ALONDRA_HOME: '/fixture', ALONDRA_DOMAIN_CAPABILITY: 'fixture-capability', ALONDRA_DOMAIN_PROTOCOL: '2', ALONDRA_SESSION_ID: 'node', ALONDRA_PROVIDER: 'codex', ALONDRA_PORT: '47811', ALONDRA_SANDBOX: '1', OPENAI_API_KEY: 'must-not-forward', ALONDRA_UNKNOWN: 'must-not-forward' } };
  const env = alondraMcpEnvironment(config);
  assert.equal(env.ALONDRA_PORT, '47811'); assert.equal(env.ALONDRA_HOME, '/fixture');
  assert.equal(env.OPENAI_API_KEY, undefined); assert.equal(env.ALONDRA_UNKNOWN, undefined);
  for (const key of ['ALONDRA_HOME', 'ALONDRA_DOMAIN_CAPABILITY']) {
    const missing = { ...config.env }; delete missing[key];
    let spawned = false;
    assert.throws(() => new CodexSession({ ...config, env: missing, executable: '/never-spawn', cwd: '/fixture', sessionId: '00000000-0000-4000-8000-000000000002', observe() {}, beforeSpawn() { spawned = true; } }), /role environment/);
    assert.equal(spawned, false);
  }
  assert.throws(() => alondraMcpEnvironment({ ...config, env: { ...config.env, ALONDRA_ORCHESTRATOR: '1' } }), /cannot inherit/);
});

test('ambiguous inherited MCP names fail closed before creating a native thread or offering input', { timeout: 15000 }, async (t) => {
  for (const name of ['inherited.danger', 'inherited"danger', 'inherited\ndanger']) {
    const profile = { model: 'fixture-model', tools: [], disallowed_tools: [], plugins: [], mcp_servers: { alondra: { command: '/safe/alondra', args: ['mcp-orchestrator'] } }, settings: { codex: { developerInstructions: 'Persistent role instructions' } } };
    const f = fixture(t, { env: { ALONDRA_FAKE_INHERITED_MCP_NAMES: JSON.stringify([name]) }, config: { orchestrator: profile } });
    f.session.offer('00000000-0000-4000-8000-000000000003', 'Must never reach a model');
    await assert.rejects(f.session.ready, /Inherited MCP server names/);
    await f.session.finished;
    assert.equal(f.messages().some((message) => ['thread/start', 'thread/resume', 'turn/start'].includes(message.method)), false);
    assert.equal(f.events.some((event) => event.type === 'decision_requested'), false);
  }
});

const apiAccount={accountRead:{account:{type:'apiKey'},requiresOpenaiAuth:true},authStatus:{authMethod:'apikey',authToken:null,requiresOpenaiAuth:true}};
const managedAccount=(workspace='fixture-workspace')=>({accountRead:{account:{type:'chatgpt',email:'first@example.test',planType:'plus'},requiresOpenaiAuth:true},authStatus:{authMethod:'chatgpt',authToken:null,requiresOpenaiAuth:true},rateLimits:{accountId:workspace,ordinaryUsageAllowed:true,rateLimits:{},rateLimitsByLimitId:null}});
test('API key without its exact original grant is refused before any native thread or input',{timeout:15000},async(t)=>{
  const f=fixture(t,{account:apiAccount});f.session.offer('00000000-0000-4000-8000-000000000091','Must not incur billable inference');
  const refusal=await f.next((event)=>method('alondra/inputRefused')(event)||method('turn/completed')(event));assert.equal(refusal.payload.params.code,'paid_authorization_required');assert.equal(refusal.payload.params.nativeSubmitted,false);
  assert(!f.messages().some((request)=>['thread/start','thread/resume','turn/start'].includes(request.method)));assert(!f.events.some(method('alondra/inputAccepted')));assert(!f.events.some(method('alondra/transportFailed')));
  assert(f.messages().filter((request)=>request.method==='getAuthStatus').every((request)=>request.params.includeToken===false && request.params.refreshToken===false));
});
test('each native turn rechecks mode and same-email workspace before submission',{timeout:15000},async(t)=>{
  const f=fixture(t,{account:managedAccount()});f.session.offer('00000000-0000-4000-8000-000000000092','First authorized input');await f.next(method('turn/completed'));
  writeFileSync(f.accountFile,JSON.stringify(managedAccount('different-workspace')));f.session.offer('00000000-0000-4000-8000-000000000093','Do not transfer this session');
  const refusal=await f.next((event)=>method('alondra/inputRefused')(event)||(method('alondra/inputAccepted')(event)&&event.payload.params.inputId.endsWith('93')));assert.equal(refusal.payload.params.code,'native_account_identity_changed');assert.equal(f.messages().filter((request)=>request.method==='turn/start').length,1);
  assert.equal(JSON.parse(readFileSync(join(f.home,'execution','native-auth-account.json'),'utf8')).workspaceId,'fixture-workspace');
});
test('a same-process API key requires an exact paid grant and cannot resume as an unverified new process',{timeout:15000},async(t)=>{
  const profile={plugins:[],settings:{codex:{nativeAccountPolicy:{paidAccountId:'default'},mcp_servers:{alondra:{command:'/unused-fixture-mcp'}}}},disallowed_tools:[]};
  const f=fixture(t,{account:apiAccount,config:{worker:profile,accountId:'default'}});f.session.offer('00000000-0000-4000-8000-000000000094','Explicitly authorized paid input');await f.next(method('turn/completed'));
  f.session.offer('00000000-0000-4000-8000-000000000095','Same process and original grant');await f.next((event)=>method('alondra/inputAccepted')(event) && event.payload.params.inputId.endsWith('95'));
  f.session.close();await f.session.finished;const second=[];let observed;const refused=new Promise((resolve)=>{observed=resolve;});const later=new CodexSession({...f.config,resume:true,observe(event){second.push(event);if(method('alondra/inputRefused')(event)||method('alondra/inputAccepted')(event))observed();return second.length;}});
  const rejected=assert.rejects(later.offer('00000000-0000-4000-8000-000000000096','Unverified key identity must not resume'),/native_api_key_resume_unverifiable/);
  try {await refused;await rejected;
  assert(second.some((event)=>method('alondra/inputRefused')(event)&&event.payload.params.code==='native_api_key_resume_unverifiable'));assert(!f.messages().some((request)=>request.method==='thread/resume'));}finally{later.close();await later.finished;}
});
test('no-auth and external authentication do not inherit subscription permission',{timeout:15000},async(t)=>{
  for(const account of [{accountRead:{account:null,requiresOpenaiAuth:false},authStatus:{authMethod:null,requiresOpenaiAuth:false}}, {...managedAccount(),authStatus:{authMethod:'chatgptAuthTokens',authToken:null,requiresOpenaiAuth:true}}]){
    const f=fixture(t,{account});f.session.offer('00000000-0000-4000-8000-000000000097','Unsupported credential mode');const result=await f.next((event)=>method('alondra/inputRefused')(event)||method('alondra/inputAccepted')(event));assert(method('alondra/inputRefused')(result));assert(!f.messages().some((request)=>request.method==='turn/start'));
  }
});

test('effective provider credential and custom role overrides cannot borrow subscription metadata',{timeout:15000},async(t)=>{
  for(const config of [{model_provider:'other'},{model_providers:{openai:{env_key:'UNRELATED_PAID_KEY'}}},{model_providers:{openai:{auth:{type:'command'}}}},{model_providers:{openai:{http_headers:{Authorization:'fixture-only'}}}},{agents:{reviewer:{config_file:'unverified-role.toml'}}}]){
    const f=fixture(t,{account:{...managedAccount(),configRead:{config}}});f.session.offer('00000000-0000-4000-8000-000000000098','Do not use another provider credential');const result=await f.next((event)=>method('alondra/inputRefused')(event)||method('alondra/inputAccepted')(event));assert(method('alondra/inputRefused')(result));assert(!f.messages().some((request)=>request.method==='thread/start'||request.method==='turn/start'));
  }
});

test('managed authentication uses the actual account schema and tolerates missing optional usage identity',{timeout:15000},async(t)=>{
  for (const account of [{...managedAccount(null)}, {...managedAccount(),rateLimitsError:true}, {...managedAccount(),rateLimits:{accountId:'unvalidated-id',rateLimits:{}}}]) {
    assert(!('workspaceRouting' in account.accountRead));
    const f=fixture(t,{account});f.session.offer('00000000-0000-4000-8000-000000000099','Positive managed authentication can start without optional quota metadata');await f.next(method('turn/completed'));
    assert.equal(JSON.parse(readFileSync(join(f.home,'execution','native-auth-account.json'),'utf8')).workspaceId,null);
    assert.equal(f.events.some(method('alondra/inputRefused')),false);assert(f.messages().some((request)=>request.method==='turn/start'));
  }
});
test('optional workspace missing-known-missing transitions never upgrade original proof or reject the same managed session',{timeout:15000},async(t)=>{
  for (const initial of [null,'fixture-workspace']) {
    const f=fixture(t,{account:managedAccount(initial)});f.session.offer('00000000-0000-4000-8000-000000000100','Original managed input');await f.next(method('turn/completed'));
    for (const [suffix,workspace] of [['101','fixture-workspace'],['102',null],['103','fixture-workspace']]) {
      writeFileSync(f.accountFile,JSON.stringify(managedAccount(workspace)));const id=`00000000-0000-4000-8000-000000000${suffix}`;
      f.session.offer(id,'Same managed session with optional metadata');await f.next((event)=>method('alondra/inputAccepted')(event)&&event.payload.params.inputId===id);
      assert.equal(JSON.parse(readFileSync(join(f.home,'execution','native-auth-account.json'),'utf8')).workspaceId,initial,'the first witness is immutable, including unknown');
    }
    writeFileSync(f.accountFile,JSON.stringify(managedAccount('different-workspace')));f.session.offer('00000000-0000-4000-8000-000000000104','Known conflicting account must not run');
    const refused=await f.next((event)=>method('alondra/inputRefused')(event)&&event.payload.params.inputId.endsWith('104'));assert.equal(refused.payload.params.code,'native_account_identity_changed');
  }
});

test('each native submission records its causal start before acceptance and public user echo',{timeout:15000},async(t)=>{
  const f=fixture(t);f.session.offer('00000000-0000-4000-8000-000000000090','Causally scoped input');await f.next(method('turn/completed'));
  const starting=f.events.findIndex(method('alondra/inputStarting'));const accepted=f.events.findIndex(method('alondra/inputAccepted'));assert(starting>=0 && starting<accepted);assert.equal(f.events[starting].payload.params.inputId,'00000000-0000-4000-8000-000000000090');assert(!('prompt' in f.events[starting].payload.params));
});

const nativeThread = '00000000-0000-4000-8000-000000000001';
function nativeMessage(f, name, params) {
  f.session.child.stdout.emit('data', Buffer.from(JSON.stringify({ method: name, params }) + '\n'));
}
function rawCellItem(f, turnId, item, threadId = nativeThread) {
  nativeMessage(f, 'rawResponseItem/completed', { threadId, turnId, item });
}
function nativeHeader(status) { return [{ type: 'input_text', text: `${status}\nWall time 0.1 seconds\nOutput:\n` }]; }
const codeCellEvents = (f) => f.events.filter((event) => event.type === 'sdk' && String(event.payload.method).startsWith('alondra/codeCell'));

// Codex keeps a yielded script after its turn and advances it only in a later turn, so a paused
// script is not work: the node rests, and interrupting never takes the native process down.
test('a Code Mode script still running after its turn is not work and interrupting keeps the process', { timeout: 15000 }, async (t) => {
  const f = fixture(t);
  await f.next(method('alondra/threadReady'));
  assert.equal(f.messages().find((m) => m.method === 'thread/start').params.experimentalRawEvents, undefined);
  nativeMessage(f, 'turn/started', { threadId: nativeThread, turn: { id: 'origin' } });
  rawCellItem(f, 'origin', { type: 'custom_tool_call', namespace: 'functions', name: 'exec', call_id: 'exec-one', input: 'await pendingTool();' });
  rawCellItem(f, 'origin', { type: 'custom_tool_call_output', call_id: 'exec-one', output: nativeHeader('Script running with cell ID 17') });
  nativeMessage(f, 'turn/completed', { threadId: nativeThread, turn: { id: 'origin', status: 'completed' } });
  await f.next(method('turn/completed'));
  assert.deepEqual(codeCellEvents(f), []);
  await f.session.interruptTurn();
  assert.equal(f.events.some((event) => event.type === 'process_exited'), false);
  f.session.offer('00000000-0000-4000-8000-000000000098', 'Go on after the interruption');
  await f.next((event) => method('turn/completed')(event) && event.payload.params.turn.id === 'native-turn-1');
  assert.equal(f.events.some(method('alondra/transportFailed')), false);
});

test('a child with a paused script settles once its turn and background terminals are done', { timeout: 15000 }, async (t) => {
  const f = fixture(t); const child = 'child-thread';
  await f.next(method('alondra/threadReady'));
  nativeMessage(f, 'turn/started', { threadId: nativeThread, turn: { id: 'parent-turn' } });
  nativeMessage(f, 'item/started', { threadId: nativeThread, item: { type: 'collabAgentToolCall', id: 'spawn-item', tool: 'spawnAgent', receiverThreadIds: [child], agentsStates: {} } });
  nativeMessage(f, 'turn/started', { threadId: child, turn: { id: 'child-turn' } });
  rawCellItem(f, 'child-turn', { type: 'custom_tool_call', name: 'exec', call_id: 'child-exec', input: 'await pendingTool()' }, child);
  rawCellItem(f, 'child-turn', { type: 'custom_tool_call_output', call_id: 'child-exec', output: nativeHeader('Script running with cell ID 1') }, child);
  nativeMessage(f, 'item/completed', { threadId: nativeThread, item: { type: 'collabAgentToolCall', id: 'spawn-item', tool: 'spawnAgent', receiverThreadIds: [child], agentsStates: { [child]: { status: 'completed' } } } });
  const quiescent = await f.next(method('alondra/childQuiescent'));
  assert.equal(quiescent.payload.params.childThreadId, child);
  assert.deepEqual(codeCellEvents(f), []);
});

const workerProfile = (instructions) => ({ model: 'fixture-model', plugins: [], disallowed_tools: [], settings: { codex: { developerInstructions: instructions, mcp_servers: { alondra: { command: '/safe/alondra', args: ['mcp-node'] } } } } });

// Production, 0.3.9: every Codex node stopped after the release, because each saved thread was
// refused for having been started under the previous release's instructions.
test('a thread saved under the instructions of the release before resumes under the current ones', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { config: { worker: workerProfile('Instructions of the release before') } });
  await f.session.ready;
  f.session.close(); await f.session.finished;
  assert.equal(f.session.failure, undefined, 'a session closed on request did not fail');
  const later = new CodexSession({ ...f.config, worker: workerProfile('Instructions of this release'), resume: true, processGeneration: 'next-release' });
  try {
    await later.ready;
    assert.equal(later.thread, nativeThread);
    const resumed = f.messages().filter((message) => message.method === 'thread/resume');
    assert.equal(resumed.length, 1);
    assert.equal(resumed[0].params.threadId, nativeThread);
    assert.equal(resumed[0].params.developerInstructions, 'Instructions of this release');
    const mapping = JSON.parse(readFileSync(join(f.home, 'native-threads', f.config.sessionId)));
    assert.equal(mapping.instructionsDigest, createHash('sha256').update('Instructions of this release').digest('hex'));
  } finally { later.close(); await later.finished; }
});

test('an input whose session could not start is refused as never offered, and the session says why it ended', { timeout: 15000 }, async (t) => {
  const f = fixture(t, { account: { failResumes: 1 }, config: { worker: workerProfile('Instructions') } });
  await f.session.ready;
  f.session.close(); await f.session.finished;
  const later = new CodexSession({ ...f.config, resume: true, processGeneration: 'failing' });
  const notOffered = (error) => error instanceof InputNotOffered && error.code === 'native_initialization_failed';
  try {
    await assert.rejects(later.offer('00000000-0000-4000-8000-000000000071', 'Continue'), notOffered);
    await later.finished;
    assert.equal(later.failure, 'native_initialization_failed');
    assert.throws(() => later.offer('00000000-0000-4000-8000-000000000072', 'Again'), notOffered);
    assert.equal(f.messages().filter((message) => message.method === 'turn/start').length, 0);
  } finally { later.close(); await later.finished; }
});

for (const [silentOn, step] of [['initialize', 'initialize'], ['thread/start', 'thread_start'], ['thread/resume', 'thread_resume']]) {
  test(`a Codex that never answers ${silentOn} refuses the input as never offered and says which step did not answer`, { timeout: 15000 }, async (t) => {
    const resume = silentOn === 'thread/resume';
    const f = fixture(t, resume ? { config: { worker: workerProfile('Instructions') } } : { config: { worker: workerProfile('Instructions'), openingTimeoutMs: 200 }, env: { ALONDRA_FAKE_SILENT_ON: silentOn } });
    let session = f.session;
    if (resume) {
      await f.session.ready;
      f.session.close(); await f.session.finished;
      session = new CodexSession({ ...f.config, env: { ...f.config.env, ALONDRA_FAKE_SILENT_ON: silentOn }, resume: true, processGeneration: 'silent', openingTimeoutMs: 200 });
    }
    const firstEvent = f.events.length;
    try {
      await assert.rejects(session.offer('00000000-0000-4000-8000-000000000081', 'Continue'), (error) => error instanceof InputNotOffered && error.code === 'native_initialization_timeout');
      await session.finished;
      assert.equal(session.failure, 'native_initialization_timeout');
      const failed = f.events.slice(firstEvent).find(method('alondra/transportFailed'));
      assert.deepEqual(failed.payload.params, { code: 'native_initialization_timeout', step });
      assert.equal(f.messages().filter((message) => message.method === 'turn/start').length, 0);
    } finally { session.close(); await session.finished; }
  });
}

for (const mode of ['complete', 'child']) {
  test(`a cold resume never reads Codex's history file and keeps ${mode} turns going`, { timeout: 15000 }, async (t) => {
    const f = fixture(t, { env: { ALONDRA_FAKE_MODE: mode } });
    await f.session.ready;
    f.session.close(); await f.session.finished;
    const firstLaterEvent = f.events.length;
    const later = new CodexSession({ ...f.config, resume: true, processGeneration: `cold-${mode}` });
    try {
    await later.ready;
    assert.equal(later.thread, nativeThread);
    later.offer('00000000-0000-4000-8000-000000000097', 'Continue the same native thread');
    await f.next((event) => f.events.indexOf(event) >= firstLaterEvent && method('turn/completed')(event) && event.payload.params.threadId === nativeThread);
    assert.equal(f.messages().filter((message) => message.method === 'thread/resume').length, 1);
    if (mode === 'child') {
      await later.mcpServerStatus();
      const quiescent = await f.next((event) => f.events.indexOf(event) >= firstLaterEvent && method('alondra/childQuiescent')(event));
      assert.equal(quiescent.payload.params.childThreadId, 'native-child');
      const started = f.events.find((event) => f.events.indexOf(event) >= firstLaterEvent && method('turn/started')(event) && event.payload.params.threadId === 'native-child');
      assert(started, 'A cold child cannot be inferred from idle status without a current-process native turn');
    }
    assert.equal(f.events.filter((event) => f.events.indexOf(event) >= firstLaterEvent && method('alondra/transportFailed')(event)).length, 0);
    assert.equal(f.messages().filter((message) => message.method === 'thread/read').length, 0);
    } finally { later.close(); await later.finished; }
  });
}
