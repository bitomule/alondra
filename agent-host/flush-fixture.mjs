import assert from 'node:assert/strict';
import { timingSafeEqual } from 'node:crypto';
import { readFileSync } from 'node:fs';
import { createConnection, Socket } from 'node:net';
import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { Channel } from './channel.js';
import { parsePayload, PROTOCOL_VERSION, SDK_VERSION } from './frame.js';
import { hostProof, ownerProof } from './ownership.js';
import { PersistentHost } from './persistent.js';

const config = loadBootstrap(fileURLToPath(import.meta.url));
let nativeSpawns = 0;
const host = new PersistentHost(config, { beforeSpawn() { nativeSpawns++; throw new Error('Pure outbox fixture cannot start native work'); } });
let client;
let lane;
let originalWrite;
let flushing;
let failure;
let tearingDown = false;
const cleanupErrors = [];
process.on('SIGTERM', () => { void host.stop().catch(() => { process.exitCode = 1; }); });
function deferred() {
  let resolve;
  const promise = new Promise((done) => { resolve = done; });
  return { promise, resolve };
}
function journalFrames() {
  const bytes = readFileSync(host.journal.path);
  const frames = [];
  let offset = 0;
  while (offset < bytes.length) {
    assert(bytes.length - offset >= 4);
    const length = bytes.readUInt32BE(offset); offset += 4;
    assert(length > 0 && length <= bytes.length - offset);
    frames.push(parsePayload(bytes.subarray(offset, offset + length))); offset += length;
  }
  return frames;
}
try {
  await host.listen();
  client = createConnection(config.socket_path);
  let rejectChannel;
  const channelFailed = new Promise((_, reject) => { rejectChannel = reject; });
  channelFailed.catch(() => {});
  const frames = [];
  const waiters = [];
  const next = (predicate) => {
    const prior = frames.find(predicate);
    return prior ? Promise.resolve(prior) : Promise.race([
      new Promise((resolve) => { waiters.push({ predicate, resolve }); }), channelFailed,
    ]);
  };
  const fail = () => { if (!tearingDown) rejectChannel(new Error('Actual Unix channel failed before the expected frame')); };
  client.once('close', fail);
  const peer = new Channel(client, (frame) => {
    frames.push(frame);
    for (let index = waiters.length - 1; index >= 0; index--) {
      const waiter = waiters[index];
      if (waiter.predicate(frame)) { waiters.splice(index, 1); waiter.resolve(frame); }
    }
  }, fail);
  const hello = (await next((frame) => frame.body.type === 'hello')).body;
  assert.equal(hello.host_release, config.manifest.host_release);
  assert.equal(hello.host_sha256, config.manifest.host_sha256);
  assert.equal(hello.sdk_version, SDK_VERSION);
  const challenge = (await next((frame) => frame.body.type === 'challenge')).body;
  assert.deepEqual(challenge.fence, config.fence);
  assert.deepEqual(challenge.execution, config.execution);
  assert.equal(challenge.host_instance_id, config.host_instance_id);
  assert.equal(challenge.executable_sha256, config.manifest.executable_sha256);
  assert.match(challenge.nonce, /^[a-f0-9]{64}$/);
  assert.equal(host.lanes.size, 1);
  lane = [...host.lanes][0];
  assert(lane.channel instanceof Channel && lane.channel.socket instanceof Socket);
  flushing = lane.flushing;
  let cleared = deferred();
  let armed = false;
  let injected = false;
  let flushingAtInjection;
  const observation = { type: 'sdk', kind: 'diagnostic', native: {}, payload: { source: 'fixture_microtask', text: 'Controlled technical outbox wakeup' }, failures: [] };
  // Preserve real flag state; insert one observation behind an otherwise empty drain.
  Object.defineProperty(lane, 'flushing', {
    configurable: true, enumerable: true,
    get() { return flushing; },
    set(value) {
      flushing = value;
      if (value && armed) {
        armed = false;
        queueMicrotask(() => {
          injected = true;
          flushingAtInjection = flushing;
          host.observe(observation);
        });
      }
      if (!value) cleared.resolve();
    },
  });
  const attach = { type: 'attach', fence: config.fence, execution: config.execution, host_instance_id: config.host_instance_id, nonce: challenge.nonce, owner_generation: 1, host_release: config.manifest.host_release, host_sha256: config.manifest.host_sha256, executable_sha256: config.manifest.executable_sha256, sdk_version: SDK_VERSION };
  attach.proof = ownerProof(Buffer.from(config.control_secret, 'hex'), attach);
  await peer.write({ protocol_version: PROTOCOL_VERSION, body: attach });
  const attached = (await next((frame) => frame.body.type === 'attached')).body;
  assert.deepEqual(attached.fence, config.fence);
  assert.deepEqual(attached.execution, config.execution);
  assert.equal(attached.host_instance_id, config.host_instance_id);
  assert.equal(attached.owner_generation, 1);
  assert(timingSafeEqual(Buffer.from(attached.proof, 'hex'), Buffer.from(hostProof(Buffer.from(config.control_secret, 'hex'), attach), 'hex')));
  await cleared.promise;
  assert.equal(host.owner, lane);
  assert.equal(host.journal.ownerGeneration, 1);
  assert.equal(host.journal.cursor, 0);
  assert.equal(host.journal.commands.size, 0);
  assert.equal(lane.flushing, false);
  let writeRequests = 0;
  let eventWrite;
  originalWrite = lane.channel.write;
  lane.channel.write = function (frame) {
    const result = originalWrite.call(this, frame);
    if (frame.body.type === 'event' && frame.body.cursor === 1) { writeRequests++; eventWrite = result; }
    return result;
  };
  cleared = deferred();
  armed = true;
  host.flush(lane);
  await cleared.promise;
  assert.equal(injected, true);
  assert.equal(host.journal.cursor, 1, 'the injected observation must be durably journaled before the send assertion');
  const persisted = journalFrames();
  assert.equal(persisted.length, 2);
  assert.equal(persisted[0].body.type, 'attached');
  assert.deepEqual(persisted[1], host.journal.events.get(1));
  if (writeRequests !== 1) process.stderr.write(`Fixture observation arrived with flushing=${flushingAtInjection}; write requests=${writeRequests}\n`);
  assert.equal(writeRequests, 1,
    'a microtask observation after an empty drain must request its frame without another trigger');
  await eventWrite;
  const delivered = await next((frame) => frame.body.type === 'event' && frame.body.cursor === 1);
  assert.deepEqual(delivered, persisted[1], 'the real framed Unix peer must receive the exact journaled event');
  assert.equal(lane.sentCursor, 1);
  assert.equal(host.journal.acknowledged, 0, 'wire delivery is not daemon SQL acknowledgement');
  assert.equal(nativeSpawns, 0);
  assert.equal(host.session, undefined);
  console.log('Actual owned Unix lane delivered the queued microtask observation without another trigger');
} catch (error) {
  failure = error;
} finally {
  tearingDown = true;
  if (lane && originalWrite) lane.channel.write = originalWrite;
  if (lane && flushing !== undefined) Object.defineProperty(lane, 'flushing', { value: flushing, writable: true, configurable: true, enumerable: true });
  client?.destroy();
  try { await host.stop(); } catch (error) { cleanupErrors.push(error); }
}
if (failure && cleanupErrors.length) throw new AggregateError([failure, ...cleanupErrors], 'Flush fixture and teardown failed');
if (failure) throw failure;
if (cleanupErrors.length) throw new AggregateError(cleanupErrors, 'Flush fixture teardown failed');
