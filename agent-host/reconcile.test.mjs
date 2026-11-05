import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { chmodSync, closeSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { Channel } from './channel.js';
import { parsePayload, PROTOCOL_VERSION, SDK_VERSION } from './frame.js';
import { ownerProof } from './ownership.js';
import { socketRoot } from './owned-host.mjs';

const root = mkdtempSync(join(socketRoot(), 'rc-'));
chmodSync(root, 0o700);
const home = join(root, 'home'); mkdirSync(home, { mode: 0o700 });
const fixture = fileURLToPath(new URL('./reconcile-fixture.mjs', import.meta.url));
const nativeFixture = fileURLToPath(new URL('./persistent-fake-claude.py', import.meta.url));
const executable = realpathSync(process.env.JS_BINARY__NODE_BINARY);
const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
const command = parsePayload(readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures/start.json'))).body;
const config = {
  state_dir: root, ownership_path: join(root, 'host.lock'), socket_path: join(root, 's'),
  host_instance_id: 'reconcile-fixture', control_secret: randomBytes(32).toString('hex'),
  fence: command.fence, execution: command.execution,
  manifest: { mode: 'node_bundle', artifact_path: realpathSync(fixture), host_release: 'reconcile-fixture', host_sha256: hash(fixture), executable_sha256: hash(executable), sdk_version: SDK_VERSION },
  native: { session_id: '00000000-0000-4000-8000-000000000010', executable: nativeFixture, cwd: root, env: { HOME: home, PATH: '/usr/bin:/bin', FIXTURE_CAPTURE: join(root, 'capture.json') } },
};
const secret = Buffer.from(config.control_secret, 'hex');

function startHost() {
  writeFileSync(config.ownership_path, JSON.stringify(config), { mode: 0o600 });
  const bootstrap = openSync(config.ownership_path, 'r');
  const environment = { ...process.env, HOME: home };
  delete environment.CLAUDE_CONFIG_DIR; delete environment.CLAUDE_HOME;
  const child = spawn(executable, [fixture], { cwd: root, env: environment, stdio: [bootstrap, 'pipe', 'pipe'] });
  closeSync(bootstrap);
  let stderr = '';
  child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-16384); });
  return { child, stderr: () => stderr };
}

async function attach(socket, generation, next, channel) {
  const challenge = (await next((frame) => frame.body.type === 'challenge')).body;
  const attach = { type: 'attach', fence: config.fence, execution: config.execution, host_instance_id: config.host_instance_id, nonce: challenge.nonce, owner_generation: generation, host_release: config.manifest.host_release, host_sha256: config.manifest.host_sha256, executable_sha256: config.manifest.executable_sha256, sdk_version: SDK_VERSION };
  attach.proof = ownerProof(secret, attach);
  await channel.write({ protocol_version: PROTOCOL_VERSION, body: attach });
  await next((frame) => frame.body.type === 'attached');
}

function lane(socket) {
  const frames = [];
  const waits = [];
  socket.on('error', () => {});
  socket.once('close', () => { for (const waiter of waits.splice(0)) waiter.reject(new Error('lane closed before expected frame')); });
  const channel = new Channel(socket, (frame) => {
    frames.push(frame);
    for (let index = waits.length - 1; index >= 0; index--) if (waits[index].predicate(frame)) waits.splice(index, 1)[0].resolve(frame);
  }, () => {});
  const next = (predicate) => {
    const old = frames.find(predicate);
    if (old) return Promise.resolve(old);
    return new Promise((resolve, reject) => waits.push({ predicate, resolve, reject }));
  };
  return { frames, channel, next, send: (body) => channel.write({ protocol_version: PROTOCOL_VERSION, body }) };
}

const start = (id, prompt) => ({ ...command, command_id: id, deadline_ms: Date.now() + 60000, action: { ...command.action, cwd: config.native.cwd, prompt } });
const receiptOf = (owner, id) => owner.next((frame) => frame.body.type === 'receipt' && frame.body.command_id === id);
const outcome = (frame) => frame.body.outcome;

const host = startHost();
let socket;
try {
  const reader = createInterface({ input: host.child.stdout });
  const exited = once(host.child, 'exit');
  const line = await Promise.race([
    once(reader, 'line').then(([value]) => value),
    exited.then((value) => { throw new Error(`host exited before listen: ${JSON.stringify(value)} ${host.stderr()}`); }),
  ]);
  assert.equal(line, 'Alondra host: socket listening');
  socket = createConnection(config.socket_path);
  const owner = lane(socket);
  await attach(socket, 1, owner.next, owner.channel);

  // The launch offers to the fake native: the owner gets a final offered receipt and never an
  // admitted one, which the host keeps as an internal uncertainty marker and does not flush.
  const launch = start('reconcile-launch', 'first prompt');
  await owner.send(launch);
  assert.deepEqual(outcome(await receiptOf(owner, 'reconcile-launch')), { status: 'accepted', phase: 'offered' }, host.stderr());
  assert(!owner.frames.some((frame) => frame.body.type === 'receipt' && outcome(frame).phase === 'admitted'), 'the admitted receipt is never flushed');

  // Re-sending the exact same command (the owner reconciling after a crash) returns its definitive
  // retained receipt again, instead of being silently deduplicated and never answered.
  owner.frames.length = 0;
  await owner.send(launch);
  assert.deepEqual(outcome(await receiptOf(owner, 'reconcile-launch')), { status: 'accepted', phase: 'offered' }, 'a re-sent command returns its retained receipt');

  socket.destroy();
  host.child.kill('SIGTERM');
  await exited;
  console.log('Host reconciliation: a re-sent command is answered with its retained receipt; admitted is never flushed');
} catch (error) {
  console.error(error, host.stderr());
  process.exitCode = 1;
} finally {
  socket?.destroy();
  if (host.child.exitCode === null && host.child.signalCode === null) host.child.kill('SIGTERM');
  rmSync(root, { recursive: true, force: true });
}
