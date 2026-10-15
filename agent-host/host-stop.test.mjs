import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes, timingSafeEqual } from 'node:crypto';
import { once } from 'node:events';
import { chmodSync, closeSync, mkdtempSync, mkdirSync, openSync, readFileSync, realpathSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { Channel } from './channel.js';
import { parsePayload, PROTOCOL_VERSION, SDK_VERSION } from './frame.js';
import { hostProof, ownerProof } from './ownership.js';

// Retain the owned short canonical directory through child exit; Darwin sun_path is bounded.
const root = mkdtempSync(join(realpathSync('/tmp'), 'as-'));
chmodSync(root, 0o700);
const home = join(root, 'home'); mkdirSync(home, { mode: 0o700 });
const fixture = fileURLToPath(new URL('./host-stop-fixture.mjs', import.meta.url));
const nativeFixture = fileURLToPath(new URL('./persistent-fake-claude.py', import.meta.url));
const executable = realpathSync(process.env.JS_BINARY__NODE_BINARY);
const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const command = parsePayload(readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures/start.json'))).body;
const config = {
  state_dir: root, ownership_path: join(root, 'host.lock'), socket_path: join(root, 's'),
  host_instance_id: 'cleanup-fixture', control_secret: randomBytes(32).toString('hex'),
  fence: command.fence, execution: command.execution,
  manifest: { mode: 'node_bundle', artifact_path: realpathSync(fixture), host_release: 'cleanup-fixture', host_sha256: hash(fixture), executable_sha256: hash(executable), sdk_version: SDK_VERSION },
  native: { session_id: '00000000-0000-4000-8000-000000000010', executable: nativeFixture, cwd: root, env: { HOME: home, PATH: '/usr/bin:/bin', FIXTURE_CAPTURE: join(root, 'capture.json') } },
};
let child;
let exited;
let socket;
let reader;
let bootstrap;
try {
  writeFileSync(config.ownership_path, JSON.stringify(config), { mode: 0o600 });
  bootstrap = openSync(config.ownership_path, 'r');
  // This Node parent transfers a bootstrap File, not a flock; kernel-lock proof belongs to Rust runtime tests.
  const environment = { ...process.env, HOME: home };
  delete environment.CLAUDE_CONFIG_DIR; delete environment.CLAUDE_HOME;
  child = spawn(executable, [fixture], { cwd: root, env: environment, stdio: [bootstrap, 'pipe', 'pipe'] });
  closeSync(bootstrap); bootstrap = undefined;
  exited = new Promise((resolve) => child.once('exit', (code, signal) => resolve({ code, signal })));
  const stderrClosed = once(child.stderr, 'close');
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-16384); });
  reader = createInterface({ input: child.stdout });
  const ready = await Promise.race([once(reader, 'line').then(([line]) => line), exited.then((value) => { throw new Error(`Fixture exited before listen: ${JSON.stringify(value)} ${stderr}`); })]);
  assert.equal(ready, 'Alondra host: socket listening');
  assert.equal(statSync(config.socket_path).mode & 0o777, 0o600, 'owned Unix socket must be0600 before readiness');
  socket = createConnection(config.socket_path);
  const frames = [];
  const waits = [];
  const next = (predicate) => {
    const old = frames.find(predicate);
    if (old) return Promise.resolve(old);
    return new Promise((resolve, reject) => { waits.push({ predicate, resolve, reject }); });
  };
  const rejectWaits = (message) => { for (const waiter of waits.splice(0)) waiter.reject(new Error(message)); };
  exited.then(() => rejectWaits('Host exited before expected frame'));
  socket.once('close', () => rejectWaits('Framed channel closed before expected frame'));
  const channel = new Channel(socket, (frame) => {
    frames.push(frame);
    for (let index = waits.length - 1; index >= 0; index--) {
      const waiter = waits[index];
      if (waiter.predicate(frame)) { waits.splice(index, 1); waiter.resolve(frame); }
    }
  }, () => {});
  const hello = (await next((frame) => frame.body.type === 'hello')).body;
  assert.equal(hello.host_sha256, config.manifest.host_sha256);
  const challenge = (await next((frame) => frame.body.type === 'challenge')).body;
  assert.deepEqual(challenge.fence, config.fence);
  assert.deepEqual(challenge.execution, config.execution);
  assert.equal(challenge.host_instance_id, config.host_instance_id);
  assert.equal(challenge.executable_sha256, config.manifest.executable_sha256);
  const attach = { type: 'attach', fence: config.fence, execution: config.execution, host_instance_id: config.host_instance_id, nonce: challenge.nonce, owner_generation: 1, host_release: config.manifest.host_release, host_sha256: config.manifest.host_sha256, executable_sha256: config.manifest.executable_sha256, sdk_version: SDK_VERSION };
  attach.proof = ownerProof(Buffer.from(config.control_secret, 'hex'), attach);
  await channel.write({ protocol_version: PROTOCOL_VERSION, body: attach });
  const attached = (await next((frame) => frame.body.type === 'attached')).body;
  assert.deepEqual(attached.fence, config.fence);
  assert.deepEqual(attached.execution, config.execution);
  assert.equal(attached.host_instance_id, config.host_instance_id);
  assert.equal(attached.owner_generation, attach.owner_generation);
  assert(timingSafeEqual(Buffer.from(attached.proof, 'hex'), Buffer.from(hostProof(Buffer.from(config.control_secret, 'hex'), attach), 'hex')));
  const start = { ...command, deadline_ms: Date.now() + 60000, action: { ...command.action, cwd: root, prompt: 'stop-cleanup' } };
  await channel.write({ protocol_version: PROTOCOL_VERSION, body: start });
  await next((frame) => frame.body.type === 'event' && frame.body.observation.type === 'sdk' && frame.body.observation.kind === 'status');
  await channel.write({ protocol_version: PROTOCOL_VERSION, body: { ...command, command_id: 'cleanup-failure-offer', deadline_ms: Date.now() + 60000, action: { type: 'offer', input_id: '00000000-0000-4000-8000-000000000011', prompt: 'native-failure-now' } } });
  const result = await exited;
  await stderrClosed;
  assert.deepEqual(result, { code: 23, signal: null }, stderr);
  assert.match(stderr, /Fixture resources released after actual native exit/);
  const capture = JSON.parse(readFileSync(join(root, 'capture.json')));
  assert(capture.args.includes(`--session-id=${config.native.session_id}`));
  const retained = statSync(config.ownership_path);
  for (const descriptor of Object.values(capture.descriptors)) {
    assert(!descriptor || descriptor.dev !== retained.dev || descriptor.ino !== retained.ino, 'native child cannot inherit retained bootstrap inode');
  }
  const secretDigest = createHash('sha256').update(config.control_secret).digest('hex');
  assert(!Object.values(capture.env_value_sha256).includes(secretDigest), 'native environment cannot receive control secret');
  console.log('Unix host actual SDK failure: shared stop promise, rejected cleanup, native OS exit and released host resources');
} finally {
  if (bootstrap !== undefined) closeSync(bootstrap);
  socket?.destroy(); reader?.close();
  if (child && child.exitCode === null && child.signalCode === null) child.kill('SIGTERM');
  if (exited) await exited;
  rmSync(root, { recursive: true, force: true });
}
