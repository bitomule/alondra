import assert from 'node:assert/strict';
import { mkdtemp, mkdir, writeFile, appendFile, readFile, rename, symlink, rm, realpath } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { CodexRolloutFollower } from './codex-rollout.js';

const THREAD = 'fixture-thread';
const record = (type, payload) => JSON.stringify({ timestamp: '2026-10-26T00:00:00Z', type, payload }) + '\n';
const exec = (call, turn = 'turn-first') => ({ type: 'custom_tool_call', name: 'exec', namespace: 'functions', call_id: call, input: 'PRIVATE_SCRIPT_MUST_NOT_ESCAPE', internal_chat_message_metadata_passthrough: { turn_id: turn } });
const output = (call, status, turn = 'turn-first', type = 'custom_tool_call_output') => ({ type, call_id: call, output: [{ type: 'input_text', text: `${status}\nWall time 0.1 seconds\nOutput:\nPRIVATE_RESULT_MUST_NOT_ESCAPE` }], internal_chat_message_metadata_passthrough: { turn_id: turn } });
async function fixture(t) {
  const home = await mkdtemp(join(tmpdir(), 'alondra-code-cell-rollout-'));
  const sessions = join(home, 'sessions'); await mkdir(sessions);
  const path = join(sessions, 'rollout-fixture.jsonl');
  await writeFile(path, record('session_meta', { id: THREAD }), { mode: 0o600 });
  const items = []; const errors = []; const followers = [];
  t.after(async () => { for (const follower of followers) await follower.close(); await rm(home, { recursive: true, force: true }); });
  return { home, path, sessions, items, errors, async open(extra = {}) {
    const follower = await CodexRolloutFollower.open({ path, codexHome: home, threadId: THREAD, onItem(value) { items.push(value); }, onError(error) { errors.push(error); }, ...extra });
    followers.push(follower); return follower;
  } };
}

test('cold resume anchors existing history and emits only new exact code-cell technical evidence', async (t) => {
  const f = await fixture(t);
  await appendFile(f.path, record('response_item', exec('dead-process-cell')));
  const follower = await f.open();
  await appendFile(f.path, record('response_item', { type: 'reasoning', content: 'PRIVATE_REASONING_MUST_NOT_ESCAPE' }) + record('response_item', exec('current')) + record('response_item', output('current', 'Script running with cell ID native-cell')));
  await follower.drain();
  assert.deepEqual(f.items, [
    { threadId: THREAD, turnId: 'turn-first', item: { type: 'custom_tool_call', call_id: 'current', name: 'exec', namespace: 'functions' } },
    { threadId: THREAD, turnId: 'turn-first', item: { type: 'custom_tool_call_output', call_id: 'current', output: 'Script running with cell ID native-cell\nWall time 0.1 seconds\nOutput:\n' } },
  ]);
  await appendFile(f.path, record('response_item', { type: 'function_call', namespace: 'functions', name: 'wait', call_id: 'wait-current', arguments: JSON.stringify({ cell_id: 'native-cell', ignored: 'PRIVATE_ARGUMENT_MUST_NOT_ESCAPE' }), internal_chat_message_metadata_passthrough: { turn_id: 'turn-later' } }) + record('response_item', output('wait-current', 'Script completed', 'turn-later', 'function_call_output')));
  await follower.drain(); await follower.drain();
  assert.equal(f.items.length, 4);
  assert.equal(f.items[2].turnId, 'turn-later');
  assert.equal(f.items[2].item.arguments, '{"cell_id":"native-cell"}');
  assert.match(f.items[3].item.output, /^Script completed\n/);
  assert(!JSON.stringify(f.items).includes('PRIVATE_'));
  assert.deepEqual(f.errors, []);
});

test('partial trailing records remain buffered and are emitted once only after their newline', async (t) => {
  const f = await fixture(t); const follower = await f.open();
  const bytes = record('response_item', exec('partial'));
  await appendFile(f.path, bytes.slice(0, -1)); await follower.drain();
  assert.equal(f.items.length, 0);
  await appendFile(f.path, '\n'); await follower.drain(); await follower.drain();
  assert.equal(f.items.length, 1); assert.equal(f.items[0].item.call_id, 'partial');
});

test('drain awaits callback completion and serializes concurrent notification drains', async (t) => {
  const f = await fixture(t); let release; let entered;
  const barrier = new Promise((resolve) => { release = resolve; });
  const ready = new Promise((resolve) => { entered = resolve; });
  const follower = await f.open({ async onItem(value) { f.items.push(value); if (value.item.call_id === 'blocked') { entered(); await barrier; } } });
  await appendFile(f.path, record('response_item', exec('blocked')) + record('response_item', exec('after')));
  const first = follower.drain(); const second = follower.drain(); await ready;
  assert.deepEqual(f.items.map((value) => value.item.call_id), ['blocked']);
  release(); await Promise.all([first, second]);
  assert.deepEqual(f.items.map((value) => value.item.call_id), ['blocked', 'after']);
});

test('watcher can discover late native results independently while explicit drain preserves ordering', async (t) => {
  const f = await fixture(t); let delivered;
  const result = new Promise((resolve) => { delivered = resolve; });
  await f.open({ onItem(value) { f.items.push(value); delivered(); } });
  await appendFile(f.path, record('response_item', exec('late')));
  await result;
  assert.equal(f.items[0].item.call_id, 'late');
});

test('malformed relevant records and callback failures are sticky actionable gaps', async (t) => {
  for (const [name, item, extra] of [
    ['missing-origin', { type: 'custom_tool_call', name: 'exec', call_id: 'x' }, {}],
    ['malformed-wait', { type: 'function_call', name: 'wait', call_id: 'w', arguments: 'not-json', internal_chat_message_metadata_passthrough: { turn_id: 'turn' } }, {}],
    ['callback', exec('callback'), { onItem() { throw new Error('PRIVATE_CALLBACK_DETAIL'); } }],
  ]) {
    await t.test(name, async (t) => {
      const f = await fixture(t); const follower = await f.open(extra);
      await appendFile(f.path, record('response_item', item));
      await assert.rejects(follower.drain(), /native work remains unresolved/);
      await assert.rejects(follower.drain(), /native work remains unresolved/);
      assert.equal(f.errors.length, 1);
      assert(!f.errors[0].message.includes('PRIVATE_'));
    });
  }
});

test('replacement and truncation never silently advance across native history gaps', async (t) => {
  for (const action of ['replacement', 'truncation', 'prefix-rewrite']) await t.test(action, async (t) => {
    const f = await fixture(t); const follower = await f.open();
    if (action === 'replacement') { await rename(f.path, f.path + '.old'); await writeFile(f.path, await readFile(f.path + '.old')); }
    else if (action === 'prefix-rewrite') await writeFile(f.path, record('session_meta', { id: 'other-threadXX' }) + record('response_item', exec('not-original')));
    else await writeFile(f.path, '');
    await assert.rejects(follower.drain(), /native work remains unresolved/);
    assert.equal(f.errors.length, 1); assert.equal(f.items.length, 0);
  });
});

test('wrong thread, escaped profile, symlinked files and incomplete incarnation anchors are refused', async (t) => {
  for (const action of ['wrong-thread', 'outside', 'symlink', 'partial-anchor']) await t.test(action, async (t) => {
    const f = await fixture(t); let extra = {};
    if (action === 'wrong-thread') extra.threadId = 'other-thread';
    if (action === 'outside') { const outside = join(f.home, 'not-sessions.jsonl'); await writeFile(outside, record('session_meta', { id: THREAD })); extra.path = outside; }
    if (action === 'symlink') { const original = f.path + '.source'; await rename(f.path, original); await symlink(original, f.path); }
    if (action === 'partial-anchor') await appendFile(f.path, '{"type":"response_item"');
    await assert.rejects(f.open(extra), /native work remains unresolved/);
  });
});

test('line and queued-byte bounds stop observation instead of dropping records', async (t) => {
  for (const kind of ['line', 'drain']) await t.test(kind, async (t) => {
    const f = await fixture(t); const follower = await f.open({ limits: { lineBytes: kind === 'line' ? 128 : 1024, chunkBytes: 32, drainBytes: kind === 'drain' ? 64 : 4096 } });
    await appendFile(f.path, record('response_item', { type: 'reasoning', content: 'x'.repeat(256) }));
    await assert.rejects(follower.drain(), /native work remains unresolved/);
    assert.equal(f.errors.length, 1); assert.equal(f.items.length, 0);
  });
});

test('current child baseline scan excludes forked history and requires its positive turn boundary', async (t) => {
  const f = await fixture(t);
  await appendFile(f.path, record('response_item', exec('parent-copy', 'parent-history-turn')) + record('event_msg', { type: 'task_started', turn_id: 'current-child-turn' }) + record('response_item', exec('child-current', 'current-child-turn')) + record('response_item', output('child-current', 'Script running with cell ID child-cell', 'current-child-turn')));
  const follower = await f.open({ anchor: { currentChildTurns: new Set(['current-child-turn']) } });
  assert.deepEqual(f.items.map((value) => value.item.call_id), ['child-current', 'child-current']);
  assert(f.items.every((value) => value.turnId === 'current-child-turn'));
  await appendFile(f.path, record('response_item', exec('later-current', 'later-native-turn'))); await follower.drain();
  assert.equal(f.items.at(-1).turnId, 'later-native-turn', 'Future turns are attributed by the host before forwarding');
  await assert.rejects(f.open({ anchor: { currentChildTurns: new Set(['unproven-turn']) } }), /native work remains unresolved/);
  await assert.rejects(f.open({ anchor: { currentChildTurns: new Set() } }), /native work remains unresolved/);
});

test('close waits for in-flight callbacks, releases the reader and is idempotent', async (t) => {
  const f = await fixture(t); let release; let entered;
  const barrier = new Promise((resolve) => { release = resolve; }); const ready = new Promise((resolve) => { entered = resolve; });
  const follower = await f.open({ async onItem() { entered(); await barrier; } });
  await appendFile(f.path, record('response_item', exec('closing')));
  const draining = follower.drain().catch((error) => { assert.equal(error.code, 'closed'); }); await ready; const closed = follower.close(); release();
  await Promise.all([draining, closed]); await follower.close();
  await assert.rejects(follower.drain(), /closed/);
});


test('a native canonical path belongs to the same selected home through a home alias only', async (t) => {
  const f = await fixture(t);
  const alias = join(f.home, 'selected-home-alias');
  await symlink(f.home, alias);
  const follower = await f.open({ codexHome: alias, path: await realpath(f.path) });
  await appendFile(f.path, record('response_item', exec('same-profile-canonical')));
  await follower.drain();
  assert.equal(f.items.length, 1);
  assert.equal(f.items[0].item.call_id, 'same-profile-canonical');
  const sibling = join(f.home, 'other-profile', 'sessions');
  await mkdir(sibling, { recursive: true });
  const other = join(sibling, 'rollout-other.jsonl');
  await writeFile(other, record('session_meta', { id: THREAD }));
  await assert.rejects(f.open({ codexHome: alias, path: other }), (error) => error.code === 'outside_profile');
});
