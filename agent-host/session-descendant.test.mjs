import assert from 'node:assert/strict';
import { ChildProcess, spawn } from 'node:child_process';
import { chmodSync, existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { connect } from 'node:net';
import { join } from 'node:path';
import { Readable } from 'node:stream';
import { fileURLToPath } from 'node:url';
import { Session } from './session.js';

const root = mkdtempSync(join(realpathSync('/tmp'), 'asd-'));
chmodSync(root, 0o700);
const fixture = fileURLToPath(new URL('./descendant-fake-claude.py', import.meta.url));
const sessionId = '00000000-0000-4000-8000-000000000210';
const observations = [];
const waiters = [];
let session;
let native;
let capture;
let failure;
const cleanupFailures = [];

function observe(observation) {
  observations.push(observation);
  for (const waiter of [...waiters]) {
    if (waiter.predicate(observation)) {
      waiters.splice(waiters.indexOf(waiter), 1);
      waiter.resolve(observation);
    }
  }
}
function observed(predicate) {
  const prior = observations.find(predicate);
  if (prior) return Promise.resolve(prior);
  return Promise.race([
    new Promise((resolve) => { waiters.push({ predicate, resolve }); }),
    session.finished.then(() => { throw new Error('SDK finished before the required fixture observation'); }),
  ]);
}
function command(text) {
  return new Promise((resolve, reject) => {
    const socket = connect(join(root, 'd'));
    let data = Buffer.alloc(0);
    socket.on('error', reject);
    socket.once('connect', () => socket.end(`${text}\n`));
    socket.on('data', (chunk) => {
      if (data.length + chunk.length > 4096) { socket.destroy(new Error('Fixture response exceeds bound')); return; }
      data = Buffer.concat([data, chunk]);
    });
    socket.once('end', () => {
      try { resolve(JSON.parse(data.toString('utf8'))); } catch (error) { reject(error); }
    });
  });
}
function completionWaiter() {
  const helper = spawn(fixture, ['--fixture-wait-completion', root], {
    cwd: root, env: { HOME: root, PATH: '/usr/bin:/bin' }, stdio: ['ignore', 'pipe', 'pipe'],
  });
  return new Promise((resolve, reject) => {
    helper.once('error', reject);
    helper.once('exit', (code, signal) => {
      try { assert.equal(code, 0); assert.equal(signal, null); resolve(); } catch (error) { reject(error); }
    });
  });
}

try {
  session = new Session({
    executable: fixture, cwd: root, sessionId,
    env: { HOME: root, PATH: '/usr/bin:/bin', FIXTURE_STATE: root },
    observe,
  });
  session.finished.catch(() => {});
  await observed((value) => value.type === 'sdk' && value.kind === 'initialized');
  // Pinned SDK internals are an inspection oracle here, never a production adapter API.
  native = session.query.transport.process;
  assert.ok(native instanceof ChildProcess, 'oracle must inspect the actual SDK native ChildProcess');
  assert.ok(Number.isSafeInteger(native.pid) && native.pid > 0);
  assert.ok(native.stdout instanceof Readable && native.stderr instanceof Readable);
  session.offer('00000000-0000-4000-8000-000000000211', 'root exit with live descendant');
  const result = await observed((value) => value.type === 'sdk' && value.kind === 'result');
  const exit = await observed((value) => value.type === 'process_exited');
  assert.deepEqual(exit, { type: 'process_exited', exit_code: 0, signal: null });
  assert.equal(native.exitCode, 0, 'root death must be actual child-exit evidence');
  assert.equal(result.payload.session_id, sessionId);
  assert.equal(result.payload.is_error, false);
  capture = JSON.parse(readFileSync(join(root, 'capture.json'), 'utf8'));
  assert.equal(capture.root_pid, native.pid);
  assert.equal(capture.home, root);
  assert.equal(capture.session_id, sessionId);
  assert.notEqual(capture.descendant_pid, native.pid);
  const before = await command('ping');
  assert.deepEqual(before, { alive: true, descendant_pid: capture.descendant_pid, stdout_broken: false, stderr_broken: false });
  const readEndsClosed = Promise.all([native.stdout, native.stderr].map((stream) => stream.closed
    ? Promise.resolve() : new Promise((resolve) => stream.once('close', resolve))));
  session.close();
  await session.finished;
  assert.deepEqual(
    { stdout: native.stdout.destroyed, stderr: native.stderr.destroyed },
    { stdout: true, stderr: true },
    'explicit technical cleanup must close owned native stdout/stderr read ends after actual root exit',
  );
  await readEndsClosed;
  const after = await command('ping');
  assert.deepEqual(after, { alive: true, descendant_pid: capture.descendant_pid, stdout_broken: true, stderr_broken: true });
  assert.equal(observations.some((value) => value.type === 'sdk' && value.kind === 'task'), false,
    'closing observation handles must not fabricate descendant work completion');
  console.log('Actual SDK root exit and pipe cleanup preserve a live independently released descendant');
} catch (error) {
  failure = error;
} finally {
  try { session?.close(); } catch (error) { cleanupFailures.push(error); }
  // RED teardown releases only this fixture's read handles, never its process group.
  native?.stdout?.destroy();
  native?.stderr?.destroy();
  if (session) {
    try { await session.finished; } catch (error) { cleanupFailures.push(error); }
  }
  if (existsSync(join(root, 'capture.json'))) {
    try {
      // Known descendant code never manually releases this lock: helper exit is the quiescence barrier.
      const quiescent = completionWaiter();
      quiescent.catch(() => {});
      assert.deepEqual(await command('stop'), { stopping: true });
      await quiescent;
    } catch (error) { cleanupFailures.push(error); }
  }
  if (cleanupFailures.length === 0) rmSync(root, { recursive: true, force: true });
}
if (failure && cleanupFailures.length) throw new AggregateError([failure, ...cleanupFailures], 'Fixture failed and cleanup also failed');
if (failure) throw failure;
if (cleanupFailures.length) throw new AggregateError(cleanupFailures, 'Fixture cleanup failed; retained fixture root');
