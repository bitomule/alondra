import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { chmodSync, closeSync, mkdtempSync, mkdirSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { parsePayload, SDK_VERSION } from './frame.js';
import { socketRoot } from './owned-host.mjs';

const root = mkdtempSync(join(socketRoot(), 'asf-'));
chmodSync(root, 0o700);
const home = join(root, 'home'); mkdirSync(home, { mode: 0o700 });
const fixture = realpathSync(fileURLToPath(new URL('./flush-fixture.mjs', import.meta.url)));
const executable = realpathSync(process.env.JS_BINARY__NODE_BINARY);
const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const start = parsePayload(readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures/start.json'))).body;
const config = {
  state_dir: root, ownership_path: join(root, 'host.lock'), socket_path: join(root, 's'),
  host_instance_id: 'flush-fixture', control_secret: randomBytes(32).toString('hex'),
  fence: start.fence, execution: start.execution,
  manifest: { mode: 'node_bundle', artifact_path: fixture, host_release: 'flush-fixture', host_sha256: hash(fixture), executable_sha256: hash(executable), sdk_version: SDK_VERSION },
  native: { session_id: '00000000-0000-4000-8000-000000000220', executable: '/fixture/native-must-not-start', cwd: root, env: { HOME: home, PATH: '/usr/bin:/bin' } },
};
let descriptor;
let child;
let exited;
let failure;
const cleanupErrors = [];
try {
  writeFileSync(config.ownership_path, JSON.stringify(config), { mode: 0o600 });
  descriptor = openSync(config.ownership_path, 'r');
  // This fixture transfers a retained bootstrap inode; it does not claim Rust kernel-lock proof.
  child = spawn(executable, [fixture], { cwd: root, env: { HOME: home, PATH: '/usr/bin:/bin' }, stdio: [descriptor, 'pipe', 'pipe'] });
  exited = new Promise((resolve) => {
    child.once('exit', (code, signal) => resolve({ code, signal }));
  });
  const processFailed = new Promise((_, reject) => child.on('error', reject));
  processFailed.catch(() => {});
  closeSync(descriptor); descriptor = undefined;
  const streamsClosed = Promise.all([once(child.stdout, 'close'), once(child.stderr, 'close')]);
  streamsClosed.catch(() => {});
  let stdout = '';
  let stderr = '';
  child.stdout.on('data', (chunk) => { stdout = (stdout + chunk).slice(-16384); });
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-16384); });
  const status = await Promise.race([exited, processFailed]);
  await streamsClosed;
  assert.deepEqual(status, { code: 0, signal: null }, stderr);
  assert.match(stdout, /Actual owned Unix lane delivered the queued microtask observation without another trigger/);
  console.log(stdout.trim());
} catch (error) {
  failure = error;
} finally {
  if (descriptor !== undefined) {
    try { closeSync(descriptor); } catch (error) { cleanupErrors.push(error); }
  }
  if (child?.pid !== undefined) {
    try {
      if (child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
    } catch (error) { cleanupErrors.push(error); }
    // A post-spawn error cannot satisfy this actual-exit barrier.
    await exited;
  }
  try { rmSync(root, { recursive: true, force: true }); } catch (error) { cleanupErrors.push(error); }
}
if (failure && cleanupErrors.length) throw new AggregateError([failure, ...cleanupErrors], 'Flush parent fixture and cleanup failed', { cause: failure });
if (failure) throw failure;
if (cleanupErrors.length) throw new AggregateError(cleanupErrors, 'Flush parent fixture cleanup failed');
