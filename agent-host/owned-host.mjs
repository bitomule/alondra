import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { chmodSync, closeSync, existsSync, mkdirSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { fileURLToPath } from 'node:url';
import { Channel } from './channel.js';
import { parsePayload, PROTOCOL_VERSION, SDK_VERSION } from './frame.js';
import { ownerProof } from './ownership.js';

/** Real persistent hosts over the persistent fake Claude, each owned through a real attach. */
export function ownedHosts(prefix) {
  const root = mkdtempSync(join(realpathSync('/tmp'), prefix));
  chmodSync(root, 0o700);
  const fixture = fileURLToPath(new URL('./controls-fixture.mjs', import.meta.url));
  const nativeFixture = fileURLToPath(new URL('./persistent-fake-claude.py', import.meta.url));
  const executable = realpathSync(process.env.JS_BINARY__NODE_BINARY);
  const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
  const wire = (name) => parsePayload(readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, `crates/agent-protocol/fixtures/${name}.json`))).body;
  const task = wire('start');
  const hosts = [];

  function startHost(name, { fence = task.fence, execution = task.execution, capabilities, profile } = {}) {
    const state = join(root, name); mkdirSync(state, { mode: 0o700 });
    const home = join(state, 'home'); mkdirSync(home, { mode: 0o700 });
    const capture = join(state, 'capture.json');
    const config = {
      state_dir: state, ownership_path: join(state, 'host.lock'), socket_path: join(state, 's'),
      host_instance_id: `${prefix}${name}`, control_secret: randomBytes(32).toString('hex'), fence, execution,
      manifest: { mode: 'node_bundle', artifact_path: realpathSync(fixture), host_release: 'controls-fixture', host_sha256: hash(fixture), executable_sha256: hash(executable), sdk_version: SDK_VERSION },
      native: { session_id: '00000000-0000-4000-8000-000000000010', executable: nativeFixture, cwd: state, env: { HOME: home, PATH: '/usr/bin:/bin', FIXTURE_CAPTURE: capture } },
      ...(profile ? { orchestrator: profile } : {}),
    };
    writeFileSync(config.ownership_path, JSON.stringify(config), { mode: 0o600 });
    const bootstrap = openSync(config.ownership_path, 'r');
    const environment = { ...process.env, HOME: home };
    delete environment.CLAUDE_CONFIG_DIR; delete environment.CLAUDE_HOME;
    if (capabilities) environment.FIXTURE_HOST_CAPABILITIES = capabilities.join(',');
    else delete environment.FIXTURE_HOST_CAPABILITIES;
    const child = spawn(executable, [fixture], { cwd: state, env: environment, stdio: [bootstrap, 'pipe', 'pipe'] });
    closeSync(bootstrap);
    let stderr = '';
    child.stderr.on('data', (chunk) => { stderr = (stderr + chunk).slice(-16384); });
    const host = { child, config, capture, stderr: () => stderr, exited: once(child, 'exit') };
    hosts.push(host);
    return host;
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

  async function own(host) {
    const reader = createInterface({ input: host.child.stdout });
    const line = await Promise.race([
      once(reader, 'line').then(([value]) => value),
      host.exited.then((value) => { throw new Error(`host exited before listen: ${JSON.stringify(value)} ${host.stderr()}`); }),
    ]);
    assert.equal(line, 'Alondra host: socket listening');
    const socket = createConnection(host.config.socket_path);
    host.socket = socket;
    const owner = lane(socket);
    const { config } = host;
    const challenge = (await owner.next((frame) => frame.body.type === 'challenge')).body;
    const attach = { type: 'attach', fence: config.fence, execution: config.execution, host_instance_id: config.host_instance_id, nonce: challenge.nonce, owner_generation: 1, host_release: config.manifest.host_release, host_sha256: config.manifest.host_sha256, executable_sha256: config.manifest.executable_sha256, sdk_version: SDK_VERSION };
    attach.proof = ownerProof(Buffer.from(config.control_secret, 'hex'), attach);
    await owner.send(attach);
    await owner.next((frame) => frame.body.type === 'attached');
    let serial = 0;
    owner.command = async (action) => {
      const id = `command-${++serial}`;
      await owner.send({ type: 'command', fence: config.fence, execution: config.execution, command_id: id, deadline_ms: Date.now() + 60000, action });
      const receipt = await owner.next((frame) => frame.body.type === 'receipt' && frame.body.command_id === id);
      return { id, outcome: receipt.body.outcome, at: owner.frames.indexOf(receipt) };
    };
    owner.event = (type, predicate = () => true) => owner.next((frame) => frame.body.type === 'event' && frame.body.observation.type === type && predicate(frame.body.observation, frame));
    owner.sdk = (kind, predicate = () => true) => owner.event('sdk', (observation) => observation.kind === kind && predicate(observation.payload));
    owner.hello = owner.frames.find((frame) => frame.body.type === 'hello').body;
    return owner;
  }

  const lines = (path) => existsSync(path) ? readFileSync(path, 'utf8').split('\n').filter(Boolean).map((line) => JSON.parse(line)) : [];

  async function cleanup() {
    for (const host of hosts) {
      host.socket?.destroy();
      if (host.child.exitCode === null && host.child.signalCode === null) host.child.kill('SIGTERM');
      await host.exited;
    }
    rmSync(root, { recursive: true, force: true });
  }

  return { wire, startHost, own, lines, cleanup, stderr: () => hosts.map((host) => host.stderr()).join('\n') };
}
