import assert from 'node:assert/strict';
import { createServer, createConnection } from 'node:net';
import { once } from 'node:events';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { Channel } from './channel.js';
import { encode, parsePayload, MAX_FRAME_BYTES } from './frame.js';
const raw = readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures/hello.json'));
const frame = parsePayload(raw);
const closed = (socket) => new Promise((resolve) => socket.closed ? resolve() : socket.once('close', resolve));
async function pair(body) {
  const server = createServer();
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const accepted = once(server, 'connection');
  const client = createConnection(server.address().port, '127.0.0.1'); client.on('error', () => {});
  const [peer] = await accepted; peer.on('error', () => {});
  try { await body(client, peer); }
  finally {
    const ends = [closed(client), closed(peer)]; client.destroy(); peer.destroy(); await Promise.all(ends);
    await new Promise((resolve) => server.close(resolve));
  }
}
await pair(async (client, peer) => {
  const observations = [];
  let delivered;
  const ready = new Promise((resolve) => { delivered = resolve; });
  new Channel(peer, (value) => { observations.push(value); if (observations.length === 2) delivered(); }, () => assert.fail('valid fragmented stream'));
  const bytes = encode(frame);
  // Event barriers force two partial header reads, without sleeps or assuming packet boundaries.
  for (const chunk of [bytes.subarray(0, 1), bytes.subarray(1, 3)]) {
    const seen = once(peer, 'data'); client.write(chunk); await seen;
  }
  client.write(Buffer.concat([bytes.subarray(3), bytes]));
  await ready; assert.deepEqual(observations, [frame, frame]);
});
await pair(async (client, peer) => {
  const values = []; let delivered;
  const ready = new Promise((resolve) => { delivered = resolve; });
  new Channel(client, (value) => { values.push(value); if (values.length === 2) delivered(); }, () => assert.fail('ordered output stream'));
  const channel = new Channel(peer, () => assert.fail('unexpected input'), () => {});
  await Promise.all([channel.write(frame), channel.write(frame)]);
  await ready; assert.deepEqual(values, [frame, frame]);
});
await pair(async (client, peer) => {
  let failed;
  const rejected = new Promise((resolve) => { failed = resolve; });
  new Channel(peer, () => assert.fail('oversized header cannot produce a frame'), failed);
  const header = Buffer.alloc(4); header.writeUInt32BE(MAX_FRAME_BYTES + 1);
  const end = closed(peer); client.write(header); await rejected; await end;
});
console.log('Bounded incremental frames, ordered concurrent writes and oversized-prefix refusal passed');
