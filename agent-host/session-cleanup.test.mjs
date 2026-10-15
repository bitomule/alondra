import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { Session } from './session.js';

const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'sdk-cleanup-'));
const capture = join(root, 'capture.json');
const fixture = fileURLToPath(new URL('./fake-claude.mjs', import.meta.url));
let child;
let exited;
let session;
let originalClose;
let closeAttempt;
const closeAttempted = new Promise((resolve) => { closeAttempt = resolve; });
const cleanupFailure = new Error('Controlled Query.close failure');
try {
  session = new Session({
    executable: '/fixture/original-claude', cwd: root,
    env: { HOME: root, FIXTURE_SCENARIO: 'exit_barrier', FIXTURE_CAPTURE: capture },
    spawn(options) {
      child = spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: root, env: options.env, stdio: ['pipe', 'pipe', 'inherit', 'pipe'] });
      exited = new Promise((resolve) => child.once('exit', resolve));
      child.kill = () => true;
      return child;
    },
    observe(observation) {
      if (observation.type === 'sdk' && observation.kind === 'status') child.emit('error', new Error('Controlled native transport failure'));
    },
  });
  // Wrap the actual SDK Query; its transport and real child still perform cleanup.
  originalClose = session.query.close.bind(session.query);
  session.query.close = () => { originalClose(); closeAttempt(); throw cleanupFailure; };
  let settled = false;
  session.finished.then(() => { settled = true; }, () => { settled = true; });
  session.offer('00000000-0000-4000-8000-000000000009', 'controlled cleanup failure');
  await closeAttempted;
  await new Promise(setImmediate);
  assert.equal(settled, false, 'Query.close failure must still wait for actual native exit');
  assert.equal(child.exitCode, null);
  assert.equal(JSON.parse(readFileSync(capture)).home, root);
  child.stdio[3].end('release');
  await exited;
  await assert.rejects(session.finished, (error) => error === cleanupFailure, 'cleanup rejection must survive the exit barrier');
  assert.equal(child.exitCode, 0);
  console.log('Actual SDK cleanup rejection retains actual native exit barrier');
} finally {
  if (session && originalClose) session.query.close = originalClose;
  if (child && child.exitCode === null) child.stdio[3].end('release');
  if (exited) await exited;
  if (session) { session.close(); await session.finished.catch(() => {}); }
  rmSync(root, { recursive: true, force: true });
}
