import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parsePayload } from './frame.js';
const start = parsePayload(readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures/start.json'))).body;

const fixture = fileURLToPath(new URL('./fake-claude.mjs', import.meta.url));
const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'sdk-'));
process.env.HOME = root;
delete process.env.CLAUDE_CONFIG_DIR;
delete process.env.CLAUDE_HOME;
let serial = 0;
try {
  const { Session, classify, PermissionCallbacks } = await import('./session.js');
  async function run(scenario, decide, inputId = start.action.input_id, afterResult) {
    const id = String(serial++);
    const home = join(root, `home-${id}`);
    mkdirSync(home);
    const observations = [];
    let session;
    let interruption;
    let completed;
    const observedResult = new Promise((resolve) => { completed = resolve; });
    const capture = join(root, `capture-${id}.json`);
    const decision = join(root, `decision-${id}.json`);
    let cursor = 0;
    session = new Session({
      executable: '/fixture/original-claude', cwd: root,
      env: { HOME: home, PATH: '/fixture/no-js-runtime', FIXTURE_SCENARIO: scenario, FIXTURE_CAPTURE: capture, FIXTURE_DECISION: decision },
      spawn(options) {
        assert.equal(options.command, '/fixture/original-claude');
        assert.equal(options.env.HOME, home);
        assert.equal(options.env.CLAUDE_CONFIG_DIR, undefined);
        return spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'inherit'] });
      },
      observe(observation) {
        observations.push(observation);
        cursor += 1;
        if (observation.type === 'sdk' && observation.kind === 'diagnostic') completed();
        if (observation.type === 'decision_requested') { const epoch = cursor; queueMicrotask(() => decide?.(session, observation, epoch)); }
        if (scenario === 'interrupt' && observation.type === 'sdk' && observation.kind === 'status') interruption = session.interrupt();
        if (observation.type === 'sdk' && observation.kind === 'result') {
          if (scenario === 'multi_eof') session.offer('00000000-0000-4000-8000-000000000003', 'second input with EOF');
          else completed();
        }
        return cursor;
      },
    });
    try {
    session.offer(inputId, start.action.prompt);
    if (scenario === 'uuid_alias') assert.throws(() => session.offer(inputId.toLowerCase(), 'duplicate spelling'), /Duplicate outstanding input/);
    if (scenario === 'eof' || scenario === 'multi_eof') await session.finished;
    else { await observedResult; afterResult?.(session); if (interruption) await interruption; session.close(); await session.finished; }
    const init = JSON.parse(readFileSync(capture));
    assert.equal(init.home, home);
    assert.equal(init.startup_failure_results, '1');
    assert(init.args.includes('--setting-sources=user,project,local'));
    assert(init.args.includes('--permission-mode=auto'));
    assert(!init.args.includes('--dangerously-skip-permissions'));
    return { observations, decision, init, session, interruptReceipt: interruption ? await interruption : undefined };
    } finally { session.close(); await session.finished; }
  }
  const callbackEvents = [];
  const callbacks = new PermissionCallbacks((event) => { callbackEvents.push(event); return callbackEvents.length; });
  const identity = (event, epoch) => ({ request_id: event.request_id, epoch, kind: event.kind, tool_use_id: event.tool_use_id ?? null, asked: event.payload });
  const oldAbort = new AbortController();
  const request = { signal: oldAbort.signal, requestId: 'duplicate', toolUseID: 'fixture-tool' };
  const first = callbacks.request('Bash', {}, request);
  const duplicate = callbacks.request('Bash', {}, request);
  assert.equal(callbackEvents.length, 1, 'duplicate must not replace or re-publish the first callback');
  assert.equal((await duplicate).behavior, 'deny');
  assert.equal(callbacks.respond(identity(callbackEvents[0], 2), { behavior: 'allow', updated_input: {} }), false, 'another epoch reaches nothing');
  assert.equal(callbacks.respond({ ...identity(callbackEvents[0], 1), kind: 'question' }, { behavior: 'allow', updated_input: {} }), false, 'another kind reaches nothing');
  assert.equal(callbacks.respond({ ...identity(callbackEvents[0], 1), tool_use_id: 'other-tool' }, { behavior: 'allow', updated_input: {} }), false, 'another tool use reaches nothing');
  assert.equal(callbacks.respond(identity(callbackEvents[0], 1), { behavior: 'accept' }), false, 'an elicitation answer never answers a permission');
  assert.equal(callbacks.respond({ ...identity(callbackEvents[0], 1), asked: { ...callbackEvents[0].payload, input: { command: 'rm -rf build' } } }, { behavior: 'allow', updated_input: {} }), false, 'an answer to another request reaches nothing');
  assert.equal(callbackEvents.length, 1, 'a refused answer settles nothing');
  assert.equal(callbacks.respond(identity(callbackEvents[0], 1), { behavior: 'allow', updated_input: { command: 'ls' } }), true);
  assert.deepEqual(await first, { behavior: 'allow', updatedInput: { command: 'ls' } });
  assert.deepEqual(callbackEvents[1], { type: 'decision_settled', request_id: 'duplicate', epoch: 1, outcome: 'answered' });
  const replacement = callbacks.request('Bash', {}, { ...request, signal: new AbortController().signal });
  oldAbort.abort();
  assert.equal(callbacks.respond(identity(callbackEvents[2], 1), { behavior: 'deny', message: 'replacement' }), false, 'the answered epoch never answers its replacement');
  assert.equal(callbacks.respond(identity(callbackEvents[2], 3), { behavior: 'deny', message: 'replacement' }), true);
  assert.equal((await replacement).message, 'replacement');
  const pending = callbacks.request('Bash', {}, { ...request, requestId: 'closing', signal: new AbortController().signal });
  callbacks.close();
  assert.equal((await pending).behavior, 'deny');
  assert.deepEqual(callbackEvents.at(-1), { type: 'decision_settled', request_id: 'closing', epoch: 5, outcome: 'cancelled' });
  assert.equal(callbacks.respond(identity(callbackEvents[4], 5), { behavior: 'allow', updated_input: {} }), false);
  for (const [tool, kind] of [['AskUserQuestion', 'question'], ['ExitPlanMode', 'plan'], ['mcp__plugin_alondra_alondra__ask', 'permission']]) {
    const events = [];
    const classified = new PermissionCallbacks((event) => { events.push(event); return events.length; });
    void classified.request(tool, { field: 'value' }, { signal: new AbortController().signal, requestId: tool, toolUseID: `${tool}-use`, title: 'Title', mcpServer: { name: 'plugin:alondra:alondra', source: 'plugin' }, blockedPath: undefined });
    assert.deepEqual(events[0], { type: 'decision_requested', request_id: tool, tool_use_id: `${tool}-use`, kind, payload: { tool, input: { field: 'value' }, request: { requestId: tool, toolUseID: `${tool}-use`, title: 'Title', mcpServer: { name: 'plugin:alondra:alondra', source: 'plugin' } } } });
    classified.close();
  }
  const unpersisted = new PermissionCallbacks(() => undefined);
  assert.deepEqual(await unpersisted.request('Bash', {}, { signal: new AbortController().signal, requestId: 'lost', toolUseID: 'lost-tool' }), { behavior: 'deny', message: 'Permission delivery failed' }, 'a request with no durable event is denied');
  const elicited = [];
  const elicitations = new PermissionCallbacks((event) => { elicited.push(event); return elicited.length; });
  const form = { serverName: 'fixture-server', message: 'Which name?', mode: 'form', requestedSchema: { type: 'object' } };
  const accepted = elicitations.elicitation(form, { signal: new AbortController().signal, requestId: 'accept' });
  assert.deepEqual(elicited[0], { type: 'decision_requested', request_id: 'accept', kind: 'elicitation', payload: form }, 'an elicitation names no tool use');
  assert.equal(elicitations.respond({ request_id: 'accept', epoch: 1, kind: 'elicitation', tool_use_id: null, asked: form }, { behavior: 'allow', updated_input: {} }), false);
  assert.equal(elicitations.respond({ request_id: 'accept', epoch: 1, kind: 'elicitation', tool_use_id: null, asked: form }, { behavior: 'accept', content: { name: 'Ada' } }), true);
  assert.deepEqual(await accepted, { action: 'accept', content: { name: 'Ada' } });
  const declined = elicitations.elicitation(form, { signal: new AbortController().signal, requestId: 'decline' });
  assert.equal(elicitations.respond({ request_id: 'decline', epoch: 3, kind: 'elicitation', tool_use_id: null, asked: form }, { behavior: 'decline' }), true);
  assert.deepEqual(await declined, { action: 'decline' });
  const aborted = new AbortController();
  const withdrawn = elicitations.elicitation(form, { signal: aborted.signal, requestId: 'withdrawn' });
  aborted.abort();
  assert.deepEqual(await withdrawn, { action: 'cancel' });
  assert.deepEqual(elicited.filter((event) => event.type === 'decision_settled').map((event) => [event.request_id, event.epoch, event.outcome]), [['accept', 1, 'answered'], ['decline', 3, 'answered'], ['withdrawn', 5, 'cancelled']]);
  let barrierChild;
  let barrierExited;
  let barrierSession;
  let diagnostic;
  const diagnosticReady = new Promise((resolve) => { diagnostic = resolve; });
  const barrierCapture = join(root, 'barrier-capture.json');
  barrierSession = new Session({
    executable: '/fixture/original-claude', cwd: root,
    env: { HOME: root, FIXTURE_SCENARIO: 'exit_barrier', FIXTURE_CAPTURE: barrierCapture },
    spawn(options) {
      barrierChild = spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: root, env: options.env, stdio: ['pipe', 'pipe', 'inherit', 'pipe'] });
      barrierExited = new Promise((resolve) => barrierChild.once('exit', resolve));
      barrierChild.kill = () => true; // The separate fixture release pipe is the sole exit barrier.
      return barrierChild;
    },
    observe(observation) {
      if (observation.type === 'sdk' && observation.kind === 'status') {
        const error = new Error('Controlled post-spawn abort');
        error.name = 'AbortError';
        barrierChild.emit('error', error);
      }
      if (observation.type === 'sdk' && observation.kind === 'diagnostic') diagnostic();
    },
  });
  barrierSession.offer('00000000-0000-4000-8000-000000000009', 'controlled exit barrier');
  let finishedBeforeExit = false;
  barrierSession.finished.then(() => { finishedBeforeExit = true; });
  try {
    await diagnosticReady;
    // A microtask drain, independent of elapsed time: consume() has reached its exit barrier.
    await new Promise(setImmediate);
    assert.equal(finishedBeforeExit, false);
    assert.equal(barrierChild.exitCode, null);
  } finally {
    barrierChild.stdio[3].end('release');
    await barrierExited;
    await barrierSession.finished;
  }
  assert.equal(barrierChild.exitCode, 0);
  const alias = await run('uuid_alias', undefined, 'ABCDEFAB-1234-4ABC-8DEF-ABCDEFABCDEF');
  assert(alias.observations.some((o) => o.type === 'sdk' && o.kind === 'result' && o.payload.user_message_uuid === 'abcdefab-1234-4abc-8def-abcdefabcdef'));
  const success = await run('success');
  const reported = (run) => run.observations.filter((observation) => observation.type === 'sdk' && observation.payload?.type === 'account');
  assert.deepEqual(reported(success), [], 'an account without a plan reports none');
  const account = await run('account');
  assert.deepEqual(reported(account).map((observation) => [observation.kind, observation.payload]),
    [['status', { type: 'account', session_id: 'fixture-session', account: { subscriptionType: 'max' } }]], 'the plan of the account Claude signed in with, and nothing else of it');
  assert(success.observations.some((o) => o.type === 'sdk' && o.kind === 'initialized' && o.payload.session_id === 'fixture-session'));
  assert(success.observations.some((o) => o.type === 'sdk' && o.kind === 'result' && o.failures.length === 0 && o.payload.user_message_uuid === start.action.input_id));
  const failedDelivery = await run('failed_delivery', undefined, start.action.input_id, (session) => {
    assert.throws(() => session.offer(start.action.input_id, 'duplicate after failed delivery'), /Duplicate outstanding input/, 'failed singular echo cannot release an outstanding send');
    for (let index = 1; index <= 63; index++) session.offer(`00000000-0000-4000-8001-${String(index).padStart(12, '0')}`, 'fill technical outstanding capacity');
    assert.throws(() => session.offer('00000000-0000-4000-8001-000000000064', 'capacity'), /Outstanding input capacity exceeded/, 'failed delivery retains its technical capacity obligation');
  });
  assert(failedDelivery.observations.some((o) => o.type === 'sdk' && o.kind === 'result' && o.payload.user_message_uuid === start.action.input_id && o.payload.user_message_uuids === undefined));
  for (const scenario of ['invalid_plural', 'duplicate_plural', 'oversized_plural', 'legacy_singular', 'foreign_consumption']) {
    await run(scenario, undefined, start.action.input_id, (session) => {
      assert.throws(() => session.offer(start.action.input_id, scenario), /Duplicate outstanding input/, `${scenario} cannot release technical capacity`);
    });
  }
  for (const scenario of ['missing_singular', 'mismatched_singular', 'invalid_singular', 'child_plural']) {
    const rejected = await run(scenario, undefined, start.action.input_id, (session) => {
      assert.throws(() => session.offer(start.action.input_id, scenario), /Duplicate outstanding input/, `${scenario} cannot release root technical capacity`);
    });
    const plural = rejected.observations.find((o) => o.type === 'sdk' && Array.isArray(o.payload.user_message_uuids));
    assert(plural, `${scenario} must reach Session through the actual SDK with plural payload intact`);
    assert.deepEqual(plural.payload.user_message_uuids, [start.action.input_id]);
    if (scenario === 'missing_singular') assert.equal(plural.payload.user_message_uuid, undefined);
    if (scenario === 'mismatched_singular') assert.notEqual(plural.payload.user_message_uuid, start.action.input_id);
    if (scenario === 'child_plural') assert.equal(plural.payload.parent_tool_use_id, 'fixture-parent');
  }
  for (const scenario of ['success', 'assistant_consumed']) {
    await run(scenario, undefined, start.action.input_id, (session) => {
      assert.doesNotThrow(() => session.offer(start.action.input_id, 'valid plural released technical capacity'));
    });
  }
  let asked;
  const permission = await run('permission', (session, observation, epoch) => {
    asked = { request_id: observation.request_id, epoch, kind: observation.kind, tool_use_id: observation.tool_use_id, asked: observation.payload };
    assert.equal(observation.request_id, 'permission-request');
    assert.equal(observation.tool_use_id, 'fixture-tool');
    assert.deepEqual(observation.payload.request.mcpServer, { name: 'fixture-server', source: 'project' });
    assert.equal(observation.payload.request.matchedAskRule.source, 'project');
    assert(session.respond(asked, { behavior: 'deny', message: 'fixture refused' }));
  });
  assert.deepEqual(JSON.parse(readFileSync(permission.decision)).response.response, { behavior: 'deny', message: 'fixture refused', toolUseID: 'fixture-tool' });
  assert.equal(permission.session.respond(asked, { behavior: 'allow', updated_input: {} }), false);
  assert.equal(permission.observations.filter((o) => o.type === 'decision_settled').length, 1);
  const cancelled = await run('cancel_permission');
  assert.equal(JSON.parse(readFileSync(cancelled.decision)).response.response.behavior, 'deny');
  assert.deepEqual(cancelled.observations.filter((o) => o.type === 'decision_settled').map((o) => o.outcome), ['cancelled']);
  const interrupted = await run('interrupt');
  assert.deepEqual(interrupted.interruptReceipt, { still_queued: [] });
  assert(interrupted.observations.some((o) => o.type === 'sdk' && o.kind === 'result' && o.failures.some((f) => f.code === 'aborted_tools')));
  const outcomes = await Promise.allSettled([run('success'), run('success')]);
  for (const outcome of outcomes) if (outcome.status === 'rejected') throw outcome.reason;
  const concurrent = outcomes.map((outcome) => outcome.value);
  assert.notEqual(concurrent[0].init.home, concurrent[1].init.home);
  assert.equal(process.env.HOME, root);
  const startup = await run('startup');
  assert(startup.observations.some((o) => o.type === 'sdk' && o.failures.some((f) => f.code === 'session_held_by_background')));
  const logout = await run('logout');
  const auth = logout.observations.filter((o) => o.type === 'sdk' && o.payload.type === 'auth_status');
  assert.equal(auth.length, 2);
  assert.equal(auth[0].failures.length, 0);
  assert.equal(auth[1].failures[0].surface, 'auth');
  assert(logout.observations.some((o) => o.type === 'sdk' && o.failures.some((f) => f.code === 'authentication_failed')));
  assert(logout.observations.some((o) => o.type === 'sdk' && o.kind === 'result' && o.failures.length > 0));
  const eof = await run('eof');
  assert(eof.observations.some((o) => o.type === 'sdk' && o.failures.some((f) => ['eof_without_result', 'iterator_error'].includes(f.code))));
  const multiEof = await run('multi_eof');
  assert(multiEof.observations.some((o) => o.type === 'sdk' && o.failures.some((f) => ['eof_without_result', 'iterator_error'].includes(f.code))));
  const unknownSystem = classify({ type: 'system', subtype: 'future_subtype', uuid: 'future' });
  assert.equal(unknownSystem.kind, 'unsupported');
  assert(classify({ type: 'system', subtype: 'mirror_error', error: 'fixture' }).failures.length > 0);
  assert(classify({ type: 'system', subtype: 'hook_response', outcome: 'error' }).failures.length > 0);
  assert(classify({ type: 'rate_limit_event', rate_limit_info: { status: 'rejected' } }).failures.length > 0);
  const unknown = { type: 'future_sdk_event', future: { data: 'preserved' } };
  assert.equal(classify(unknown).payload, unknown);
  assert.equal(classify(unknown).failures[0].code, 'unknown_sdk_message');
  console.log('actual SDK query + fake Claude: defaults, input, permission denial, stale decision, interrupt, startup, logout, result error, EOF passed');
} finally {
  rmSync(root, { recursive: true, force: true });
}
