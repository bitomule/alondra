import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { encode, decode, commandDigest, parsePayload, MAX_FRAME_BYTES } from './frame.js';
process.env.HOME = process.env.TEST_TMPDIR;
delete process.env.CLAUDE_CONFIG_DIR;
delete process.env.CLAUDE_HOME;
const { classify } = await import('./session.js');

const fixture = (name) => readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures', `${name}.json`));
for (const name of ['start', 'decision', 'hello', 'receipt', 'event']) {
  const bytes = fixture(name);
  const frame = parsePayload(bytes);
  const encoded = encode(frame);
  assert.equal(encoded.readUInt32BE(), bytes.length);
  assert.deepEqual(encoded.subarray(4), bytes);
  assert.deepEqual(decode(encoded), frame);
  assert.throws(() => decode(encoded.subarray(0, -1)));
}
assert.equal(commandDigest(fixture('start')), JSON.parse(fixture('receipt')).body.command_sha256);
for (const name of ['integer-tokens', 'opaque-objects', 'nesting-depth']) {
  const corpus = JSON.parse(fixture(name));
  for (const entry of corpus) {
    if (entry.accepted) {
      const parsed = parsePayload(Buffer.from(entry.json));
      assert.deepEqual(parsed, JSON.parse(entry.json), `${entry.name}: preserve opaque content`);
      assert.deepEqual(decode(encode(parsed)), JSON.parse(entry.json), `${entry.name}: roundtrip opaque content`);
    }
    else assert.throws(() => parsePayload(Buffer.from(entry.json)), undefined, entry.name);
  }
}
for (const [name, paths] of [
  ['start', [[], ['body'], ['body', 'fence'], ['body', 'execution'], ['body', 'action']]],
  ['decision', [['body', 'action', 'decision']]],
  ['receipt', [['body', 'outcome']]],
  ['event', [['body', 'observation'], ['body', 'observation', 'native'], ['body', 'observation', 'failures', 0]]],
]) {
  for (const path of paths) for (const key of ['__proto__', 'constructor', 'toString']) {
    const invalid = JSON.parse(fixture(name));
    const object = path.reduce((value, field) => value[field], invalid);
    Object.defineProperty(object, key, { value: { arbitrary: 'opaque keys belong only inside payloads' }, enumerable: true });
    assert.throws(() => parsePayload(Buffer.from(JSON.stringify(invalid))), undefined, `${name}:${path.join('.')}:${key}`);
  }
}
const start = JSON.parse(fixture('start'));
const reordered = Buffer.from(JSON.stringify({ body: start.body, protocol_version: start.protocol_version }));
assert.notEqual(commandDigest(reordered), commandDigest(fixture('start')));
assert.throws(() => parsePayload(Buffer.from(JSON.stringify({ ...start, protocol_version: 2 }))));
assert.throws(() => parsePayload(Buffer.from(JSON.stringify({ ...start, unknown: true }))));
assert.throws(() => parsePayload(Buffer.from(JSON.stringify({ ...start, body: { ...start.body, action: { ...start.body.action, input_id: 'input-test' } } }))));
assert.throws(() => parsePayload(Buffer.from(JSON.stringify({ ...start, body: { ...start.body, execution: { ...start.body.execution, generation: Number.MAX_SAFE_INTEGER + 1 } } }))));
assert.throws(() => parsePayload(Buffer.from([0xff])));
assert.throws(() => parsePayload(Buffer.from('{"protocol_version":1,"protocol_version":1,"body":{}}')));
assert.throws(() => parsePayload(Buffer.from('{"protocol_version":1,"body":"\\ud800"}')));
assert.throws(() => decode(Buffer.alloc(MAX_FRAME_BYTES + 5)));
assert.throws(() => parsePayload(Buffer.from('{"protocol_version":1,"body":' + '['.repeat(2000) + '0' + ']'.repeat(2000) + '}')), /JSON depth/);
assert(classify({ type: 'result', subtype: 'success', is_error: true }).failures.some((f) => f.code === 'success_is_error'));
assert(classify({ type: 'result', subtype: 'success', is_error: false, terminal_reason: 'aborted_tools' }).failures.some((f) => f.surface === 'terminal' && f.code === 'aborted_tools'));
assert.equal(classify({ type: 'rate_limit_event', rate_limit_info: { status: 'future_status' } }).kind, 'unsupported');
assert(classify({ type: 'result', subtype: 'success', is_error: false, terminal_reason: 'future_reason' }).failures.some((f) => f.surface === 'terminal' && f.code === 'future_reason'));
for (const terminal_reason of ['completed', 'background_requested', 'tool_deferred', undefined]) {
  assert.equal(classify({ type: 'result', subtype: 'success', is_error: false, ...(terminal_reason ? { terminal_reason } : {}) }).failures.length, 0);
}
const oversized = structuredClone(start);
oversized.body.action.prompt = 'x'.repeat(MAX_FRAME_BYTES);
const stringify = JSON.stringify;
try {
  JSON.stringify = () => { throw new Error('Unbounded serializer invoked'); };
  assert.throws(() => encode(oversized), /Frame size/);
} finally { JSON.stringify = stringify; }
const nestedOversize = structuredClone(start);
nestedOversize.body.action = { type: 'respond', request_id: 'fixture', decision: { behavior: 'allow', updated_input: { nested: Array.from({ length: 1000 }, () => 'x'.repeat(2000)) } } };
assert.throws(() => encode(nestedOversize), /Frame size/);
let getterCalled = false;
const accessor = structuredClone(start);
Object.defineProperty(accessor.body, 'action', { enumerable: true, get() { getterCalled = true; throw new Error('getter executed'); } });
assert.throws(() => encode(accessor), /Plain JSON/);
assert.equal(getterCalled, false);
let toJSONCalled = false;
const custom = structuredClone(start);
Object.defineProperty(custom, 'toJSON', { value() { toJSONCalled = true; return start; } });
assert.throws(() => encode(custom), /Plain acyclic JSON/);
assert.equal(toJSONCalled, false);
let hiddenGetterCalled = false;
const hidden = structuredClone(start);
Object.defineProperty(hidden.body, 'ignored', { get() { hiddenGetterCalled = true; throw new Error('Hidden getter executed'); } });
assert.deepEqual(decode(encode(hidden)), start);
assert.equal(hiddenGetterCalled, false);
const serializerAccessor = structuredClone(start);
Object.defineProperty(serializerAccessor, 'toJSON', { get() { toJSONCalled = true; throw new Error('Serializer getter executed'); } });
assert.throws(() => encode(serializerAccessor), /Plain acyclic JSON/);
assert.equal(toJSONCalled, false);
const catalog = JSON.parse(fixture('sdk-errors'));
for (const code of catalog.SDKAssistantMessageError) assert(classify({ type: 'assistant', error: code }).failures.some((f) => f.surface === 'assistant' && f.code === code));
for (const code of catalog.SDKStartupFailureReason) assert(classify({ type: 'result', subtype: 'error_during_execution', is_error: true, startup_failure_reason: code }).failures.some((f) => f.surface === 'startup' && f.code === code));
for (const code of catalog.SDKResultErrorSubtype) assert(classify({ type: 'result', subtype: code, is_error: true }).failures.some((f) => f.surface === 'result' && f.code === code));
for (const code of catalog.TerminalReason) assert(classify({ type: 'result', subtype: 'success', is_error: true, terminal_reason: code }).failures.some((f) => f.surface === 'terminal' && f.code === code));
console.log('Rust-shared frame bytes/digest and inspected SDK error catalog: pass');
