import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { Journal } from './journal.js';
import { parsePayload, commandDigest, PROTOCOL_VERSION } from './frame.js';
const fixture = (name) => readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures', `${name}.json`));
const start = parsePayload(fixture('start'));
const event = parsePayload(fixture('event'));
const identity = { fence: start.body.fence, execution: start.body.execution, host_instance_id: 'host-instance-fixture' };
const frame = (body) => ({ protocol_version: PROTOCOL_VERSION, body });
const validate = (value) => { assert.deepEqual(value.body.fence, identity.fence); assert.deepEqual(value.body.execution, identity.execution); };
const MiB = 1024 * 1024;
const attached = (generation) => frame({ type: 'attached', ...identity, owner_generation: generation, proof: '0'.repeat(64) });
const acknowledge = (generation, cursor) => frame({ type: 'acknowledge', ...identity, owner_generation: generation, cursor });
const observed = (cursor, bytes) => frame({ ...event.body, cursor, observation: { ...event.body.observation, payload: { type: 'assistant', session_id: 'conversation-test', text: `${cursor}`.padEnd(bytes, 'x') } } });
const command = (id) => { const raw = Buffer.from(JSON.stringify(frame({ ...start.body, command_id: id }))); return { frame: parsePayload(raw), raw }; };
const receipt = (sent, outcome) => frame({ type: 'receipt', fence: identity.fence, execution: identity.execution, command_id: sent.frame.body.command_id, command_sha256: commandDigest(sent.raw), outcome });
const admitted = { status: 'accepted', phase: 'admitted' };
const offered = { status: 'accepted', phase: 'offered' };
const directories = [];
const directory = (name) => { const path = mkdtempSync(join(process.env.TEST_TMPDIR, `${name}-`)); directories.push(path); return path; };
const journals = [];
const open = (path, ...rest) => { const journal = new Journal(path, validate, ...rest); journals.push(journal); return journal; };
try {
  {
    // A long node: tens of MiB of acknowledged evidence, owner changes, a command still unresolved.
    const path = directory('journal-long');
    const journal = open(path);
    journal.append(attached(1));
    const first = command('first'); journal.append(first.frame, first.raw); journal.append(receipt(first, admitted)); journal.append(receipt(first, offered));
    const held = command('held'); journal.append(held.frame, held.raw); journal.append(receipt(held, admitted));
    let written = 0;
    for (let cursor = 1; cursor <= 24; cursor++) {
      if (cursor === 12) journal.append(attached(2));
      journal.append(observed(cursor, 900_000)); written += 900_000;
      journal.append(acknowledge(cursor < 12 ? 1 : 2, cursor));
    }
    journal.append(observed(25, 900_000)); journal.append(observed(26, 900_000));
    assert.ok(written > 20 * MiB);
    assert.ok(statSync(journal.path).size < 8 * MiB, `the journal is compacted, not ${statSync(journal.path).size} bytes`);
    assert.equal(existsSync(join(path, 'outbox.next')), false, 'a finished compaction leaves no rewrite behind');
    assert.deepEqual([...journal.commands.keys()], ['held'], 'only the unresolved command is kept');
    assert.deepEqual(journal.settled.get('first').receipt.body.outcome, offered);
    journal.close(); journals.pop();
    const reopened = open(path);
    assert.equal(reopened.recovering, true);
    assert.equal(reopened.cursor, 26); assert.equal(reopened.acknowledged, 24); assert.equal(reopened.ownerGeneration, 2);
    assert.deepEqual([...reopened.events.keys()], [25, 26], 'every unacknowledged event survives');
    assert.deepEqual(reopened.events.get(25), observed(25, 900_000));
    assert.deepEqual([...reopened.commands.keys()], ['held']);
    assert.equal(reopened.commands.get('held').raw.toString(), held.raw.toString(), 'the unresolved command keeps its exact bytes');
    assert.deepEqual(reopened.commands.get('held').receipt.body.outcome, admitted);
    reopened.append(acknowledge(2, 26));
    reopened.append(observed(27, 10));
    assert.equal(reopened.cursor, 27, 'the compacted journal goes on from its cursor');
  }
  {
    // More commands over a node's life than the host can hold at once.
    const journal = open(directory('journal-commands'));
    journal.append(attached(1));
    for (let index = 0; index < 300; index++) {
      const sent = command(`command-${index}`);
      journal.append(sent.frame, sent.raw); journal.append(receipt(sent, admitted)); journal.append(receipt(sent, offered));
    }
    assert.equal(journal.commands.size, 1, 'each command settles once the owner sends the next');
    assert.equal(journal.settled.size, 299);
    assert.throws(() => journal.append(frame({ ...start.body, command_id: 'command-0', deadline_ms: start.body.deadline_ms + 1 })), /conflict/, 'a settled command keeps its identity');
    const held = [];
    for (let index = 0; index < 256; index++) { const sent = command(`held-${index}`); journal.append(sent.frame, sent.raw); journal.append(receipt(sent, admitted)); held.push(sent); }
    const extra = command('one-too-many');
    assert.throws(() => journal.append(extra.frame, extra.raw), /capacity/, 'unresolved commands stay bounded');
  }
  {
    // Evidence the daemon has not acknowledged is never dropped: past the bound the host takes no more.
    const journal = open(directory('journal-retained'), 0, { retainedBytes: 4 * MiB });
    journal.append(attached(1));
    for (let cursor = 1; cursor <= 4; cursor++) journal.append(observed(cursor, 900_000));
    assert.throws(() => journal.append(observed(5, 900_000)), /capacity/);
    journal.append(acknowledge(1, 4));
    for (let cursor = 5; cursor <= 8; cursor++) journal.append(observed(cursor, 900_000));
    assert.equal(journal.cursor, 8, 'acknowledged evidence no longer counts');
  }
  {
    // A compaction cut short leaves its rewrite next to the journal: the journal itself is intact.
    const path = directory('journal-cut');
    let journal = open(path);
    journal.append(attached(1)); journal.append(observed(1, 100)); journal.close(); journals.pop();
    writeFileSync(join(path, 'outbox.next'), 'cut short', { mode: 0o600 });
    journal = open(path);
    assert.equal(journal.cursor, 1); assert.equal(journal.events.size, 1);
    assert.equal(existsSync(join(path, 'outbox.next')), false);
  }
  console.log('The host journal keeps what is unacknowledged or unresolved and nothing else, for any length of node');
} finally {
  for (const journal of journals) journal.close();
  for (const path of directories) rmSync(path, { recursive: true, force: true });
}
