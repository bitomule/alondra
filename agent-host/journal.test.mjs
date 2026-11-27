import assert from 'node:assert/strict';
import { appendFileSync, mkdtempSync, readFileSync, rmSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { Journal } from './journal.js';
import { encode, parsePayload, commandDigest, PROTOCOL_VERSION } from './frame.js';
const fixture = (name) => readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures', `${name}.json`));
const original = fixture('start');
const start = parsePayload(original);
const event = parsePayload(fixture('event'));
const identity = { fence: start.body.fence, execution: start.body.execution, host_instance_id: 'host-instance-fixture' };
const frame = (body) => ({ protocol_version: PROTOCOL_VERSION, body });
const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'journal-'));
let journal;
try {
  const validate = (value) => { assert.deepEqual(value.body.fence, identity.fence); assert.deepEqual(value.body.execution, identity.execution); };
  journal = new Journal(root, validate);
  journal.append(frame({ type: 'attached', ...identity, owner_generation: 1, proof: '0'.repeat(64) }));
  journal.append(start, original);
  const prompt = start.body.action.prompt;
  start.body.action.prompt = 'Caller changed after append';
  assert.equal(journal.commands.get(start.body.command_id).command.action.prompt, prompt, 'durable command owns its decoded bytes');
  start.body.action.prompt = prompt;
  const receipt = { type: 'receipt', fence: identity.fence, execution: identity.execution, command_id: start.body.command_id, command_sha256: commandDigest(original), outcome: { status: 'accepted', phase: 'admitted' } };
  journal.append(frame(receipt));
  journal.append(frame({ ...receipt, outcome: { status: 'accepted', phase: 'offered' } }));
  journal.append(event);
  event.body.observation.payload.mutated_after_append = true;
  assert.deepEqual(journal.events.get(event.body.cursor), parsePayload(fixture('event')), 'SDK mutation cannot change retained durable replay');
  delete event.body.observation.payload.mutated_after_append;
  const bytes = statSync(journal.path).size;
  assert.throws(() => journal.append(frame({ ...receipt, outcome: { status: 'rejected', code: 'late', message: 'Cannot erase possible handoff' } })), /handoff/);
  assert.throws(() => journal.append(frame({ ...event.body, cursor: 3 })), /cursor/);
  assert.throws(() => journal.append(frame({ type: 'acknowledge', ...identity, owner_generation: 1, cursor: 2 })), /cursor/);
  assert.equal(statSync(journal.path).size, bytes, 'invalid facts must not reach durable journal');
  journal.append(frame({ type: 'acknowledge', ...identity, owner_generation: 1, cursor: 1 }));
  assert.equal(journal.events.size, 0);
  journal.close(); journal = undefined; journal = new Journal(root, validate);
  assert.equal(journal.recovering, true, 'previous commands cannot be replayed into SDK after host restart');
  assert.equal(journal.cursor, 1); assert.equal(journal.acknowledged, 1); assert.equal(journal.ownerGeneration, 1);
  assert.equal(journal.commands.size, 0, 'the ACK after its final receipt settles the command');
  assert.equal(journal.settled.get(start.body.command_id).digest, commandDigest(original));
  assert.deepEqual(journal.settled.get(start.body.command_id).receipt.body.outcome, { status: 'accepted', phase: 'offered' });
  assert.equal(journal.events.size, 0);
  const before = statSync(journal.path).size;
  assert.throws(() => journal.append(frame({ type: 'attached', ...identity, owner_generation: 1, proof: '0'.repeat(64) })), /generation/);
  assert.throws(() => journal.append(frame({ ...start.body, fence: { ...identity.fence, account_id: 'wrong' } })), /deep-equal/);
  assert.equal(statSync(journal.path).size, before);
  const continued = mkdtempSync(join(process.env.TEST_TMPDIR, 'journal-continued-'));
  let next = new Journal(continued, validate, 4);
  try {
    assert.equal(next.recovering, false, 'a host launched after the previous one ended starts with nothing to reconcile');
    assert.throws(() => next.append(event), /cursor/, 'its events continue from the previous host, never from 1');
    next.append(frame({ ...event.body, cursor: 5 }));
    next.close(); next = new Journal(continued, validate, 4);
    assert.equal(next.cursor, 5, 'replay continues from the same base');
  } finally { next.close(); rmSync(continued, { recursive: true, force: true }); }
  // 27 November: the disk filled and a write of event 465 landed but its fsync failed. The host
  // never counted that event and wrote its exit under cursor 465 too, and no daemon could read the
  // journal again. A write that fails leaves the journal taking nothing more, whatever of it landed.
  for (const landed of [1, 0.5]) {
    const full = mkdtempSync(join(process.env.TEST_TMPDIR, 'journal-full-'));
    let failing = false;
    const write = (fd, bytes) => {
      appendFileSync(fd, failing ? bytes.subarray(0, Math.floor(bytes.length * landed)) : bytes);
      if (failing) throw Object.assign(new Error('ENOSPC: no space left on device, fsync'), { code: 'ENOSPC' });
    };
    const damaged = new Journal(full, validate, 0, {}, write);
    try {
      damaged.append(frame({ type: 'attached', ...identity, owner_generation: 1, proof: '0'.repeat(64) }));
      damaged.append(frame({ ...event.body, cursor: 1 }));
      failing = true;
      assert.throws(() => damaged.append(frame({ ...event.body, cursor: 2 })), /ENOSPC/);
      failing = false;
      assert.equal(damaged.cursor, 1, 'the event whose write failed is not counted');
      assert.throws(() => damaged.append(frame({ ...event.body, cursor: 2 })), /failed before/, 'nothing follows a write whose end is unknown');
      assert.throws(() => damaged.append(frame({ type: 'acknowledge', ...identity, owner_generation: 1, cursor: 1 })), /failed before/);
      const raw = readFileSync(damaged.path); const cursors = []; let offset = 0;
      while (raw.length - offset >= 4 && raw.length - offset - 4 >= raw.readUInt32BE(offset)) {
        const length = raw.readUInt32BE(offset);
        const body = parsePayload(raw.subarray(offset + 4, offset + 4 + length)).body;
        if (body.type === 'event') cursors.push(body.cursor);
        offset += 4 + length;
      }
      assert.deepEqual(cursors, landed === 1 ? [1, 2] : [1], 'no cursor is written twice');
      assert.equal(raw.length - offset, landed === 1 ? 0 : Math.floor(encode(frame({ ...event.body, cursor: 2 })).length * landed), 'only the cut write is left at its end');
    } finally { damaged.close(); rmSync(full, { recursive: true, force: true }); }
  }
  console.log('Technical journal retains exact command and uncertainty; ACK only releases persisted observations');
} finally { journal?.close(); rmSync(root, { recursive: true, force: true }); }
