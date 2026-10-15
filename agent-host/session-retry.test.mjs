import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Session, classify } from './session.js';

// Adapter safety control only: SDK 0.3.300 declares no system/settings_error stream subtype.
const ordinaryError = { type: 'system', subtype: 'status', status: null, error: 'Fixture settings parse error' };
assert.deepEqual(classify(ordinaryError).failures, [{ surface: 'settings', code: 'settings_parse_error', details: ordinaryError }]);

async function scenario(name) {

const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'sdk-retry-'));
const fixture = fileURLToPath(new URL('./fake-claude.mjs', import.meta.url));
const first = '00000000-0000-4000-8000-000000000001';
const second = '00000000-0000-4000-8000-000000000002';
const observations = [];
let child;
const children = [];
let failure;
const cleanupErrors = [];
let spawns = 0;
let retryReady;
let resultReady;
const retry = new Promise((resolve) => { retryReady = resolve; });
const result = new Promise((resolve) => { resultReady = resolve; });
let session;
try {
  session = new Session({
    executable: '/fixture/original-claude', cwd: root,
    env: { HOME: root, FIXTURE_SCENARIO: name, FIXTURE_CAPTURE: join(root, 'capture.json') },
    spawn(options) {
      spawns++;
      child = spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: root, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'inherit'] });
      const started = child;
      const exited = new Promise((resolve) => started.once('exit', resolve));
      // Process errors never satisfy the independent actual-exit cleanup barrier.
      started.on('error', () => {});
      children.push({ child: started, exited });
      return child;
    },
    observe(observation) {
      observations.push(observation);
      if (observation.type === 'sdk' && observation.payload.subtype === 'api_retry') retryReady(observation);
      if (observation.type === 'sdk' && observation.kind === 'result') resultReady(observation);
    },
  });
  session.finished.catch(() => {});
  const beforeFinish = (event) => Promise.race([event, session.finished.then(() => { throw new Error('SDK finished before required retry fixture event'); })]);
  session.offer(first, 'retryable native provider failure');
  const progress = await beforeFinish(retry);
  assert.equal(progress.kind, 'status');
  assert.deepEqual(progress.failures, [], 'provider-owned api_retry must remain telemetry, not a settings/auth failure');
  assert.equal(progress.payload.error, name === 'api_retry' ? 'authentication_failed' : 'server_error');
  assert.equal(progress.payload.error_status, name === 'api_retry' ? 401 : null);
  assert.equal(progress.payload.max_retries, 3);
  assert.equal(progress.payload.retry_delay_ms, 1000);
  if (name === 'api_retry_connection') assert.deepEqual(progress.payload.no_response, { waited_ms: 10, retry_wait_ms: 20 });
  assert.equal(progress.payload.attempt, 1);
  assert.equal(child.exitCode, null, 'the actual native child remains alive during its retry');
  assert.equal(spawns, 1);
  assert(!observations.some((observation) => observation.type === 'process_exited' || observation.type === 'decision_requested'));
  assert(!observations.some((observation) => observation.type === 'sdk' && observation.payload.type === 'auth_status'));
  const pid = child.pid;
  // The second actual input is the fake child's causal release barrier; no sleep or timer drives it.
  session.offer(second, 'continue the same native query after retry');
  const completed = await beforeFinish(result);
  assert.equal(completed.payload.subtype, 'success');
  assert.deepEqual(completed.failures, []);
  assert.deepEqual(completed.payload.user_message_uuids, [first, second]);
  assert.equal(child.pid, pid);
  assert.equal(child.exitCode, null);
  assert.equal(spawns, 1);
  assert(!observations.some((observation) => observation.type === 'process_exited'));
  console.log('Actual SDK retry telemetry preserved; same native child survives and returns result-only success');
} catch (error) {
  failure = error;
} finally {
  let closeFailed = false;
  if (session) {
    try { session.close(); } catch (error) { cleanupErrors.push(error); closeFailed = true; }
  }
  const stopLiveFixtureChildren = () => {
    for (const record of children) {
      if (record.child.pid !== undefined && record.child.exitCode === null && record.child.signalCode === null) {
        try { record.child.kill('SIGTERM'); } catch (error) { cleanupErrors.push(error); }
      }
    }
  };
  if (!session || closeFailed) stopLiveFixtureChildren();
  if (session) {
    try { await session.finished; } catch (error) { cleanupErrors.push(error); }
  }
  stopLiveFixtureChildren();
  for (const record of children) {
    if (record.child.pid !== undefined) await record.exited;
  }
  try { rmSync(root, { recursive: true, force: true }); } catch (error) { cleanupErrors.push(error); }
}
if (failure && cleanupErrors.length) throw new AggregateError([failure, ...cleanupErrors], 'Retry fixture and cleanup failed', { cause: failure });
if (failure) throw failure;
if (cleanupErrors.length) throw new AggregateError(cleanupErrors, 'Retry fixture cleanup failed');
}
await scenario('api_retry');
await scenario('api_retry_connection');
