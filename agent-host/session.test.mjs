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
        if (observation.type === 'sdk' && observation.kind === 'diagnostic') completed();
        if (observation.type === 'decision_requested') decide?.(session, observation);
        if (scenario === 'interrupt' && observation.type === 'sdk' && observation.kind === 'status') interruption = session.interrupt();
        if (observation.type === 'sdk' && observation.kind === 'result') {
          if (scenario === 'multi_eof') session.offer('00000000-0000-4000-8000-000000000003', 'second input with EOF');
          else completed();
        }
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
    assert(init.args.includes('--permission-mode=default'));
    assert(!init.args.includes('--dangerously-skip-permissions'));
    return { observations, decision, init, session, interruptReceipt: interruption ? await interruption : undefined };
    } finally { session.close(); await session.finished; }
  }
  const callbackEvents = [];
  const callbacks = new PermissionCallbacks((event) => callbackEvents.push(event));
  const oldAbort = new AbortController();
  const request = { signal: oldAbort.signal, requestId: 'duplicate', toolUseID: 'fixture-tool' };
  const first = callbacks.request('Bash', {}, request);
  const duplicate = callbacks.request('Bash', {}, request);
  assert.equal(callbackEvents.length, 1, 'duplicate must not replace or re-publish the first callback');
  assert.equal((await duplicate).behavior, 'deny');
  callbacks.respond('duplicate', { behavior: 'allow' });
  assert.equal((await first).behavior, 'allow');
  const replacement = callbacks.request('Bash', {}, { ...request, signal: new AbortController().signal });
  oldAbort.abort();
  callbacks.respond('duplicate', { behavior: 'deny', message: 'replacement' });
  assert.equal((await replacement).message, 'replacement');
  const pending = callbacks.request('Bash', {}, { ...request, requestId: 'closing', signal: new AbortController().signal });
  callbacks.close();
  assert.equal((await pending).behavior, 'deny');
  assert.throws(() => callbacks.respond('closing', { behavior: 'allow' }), /expired/);
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
  const permission = await run('permission', (session, observation) => {
    assert.equal(observation.request_id, 'permission-request');
    assert.equal(observation.tool_use_id, 'fixture-tool');
    assert.deepEqual(observation.payload.request.mcpServer, { name: 'fixture-server', source: 'project' });
    assert.equal(observation.payload.request.matchedAskRule.source, 'project');
    session.respond(observation.request_id, { behavior: 'deny', message: 'fixture refused' });
  });
  assert.deepEqual(JSON.parse(readFileSync(permission.decision)).response.response, { behavior: 'deny', message: 'fixture refused', toolUseID: 'fixture-tool' });
  assert.throws(() => permission.session.respond('permission-request', { behavior: 'allow' }), /expired/);
  const cancelled = await run('cancel_permission');
  assert.equal(JSON.parse(readFileSync(cancelled.decision)).response.response.behavior, 'deny');
  assert.throws(() => cancelled.session.respond('permission-request', { behavior: 'allow' }), /expired/);
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
