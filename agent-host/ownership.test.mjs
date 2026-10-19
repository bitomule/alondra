import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import { ownerProof, hostProof, proofMatches } from './ownership.js';
import { parsePayload } from './frame.js';
const fixture = (name) => readFileSync(join(process.env.TEST_SRCDIR, process.env.TEST_WORKSPACE, 'crates/agent-protocol/fixtures', `${name}.json`));
const vector = JSON.parse(fixture('owner-proof'));
const frame = parsePayload(fixture('attach'));
assert.equal(frame.body.type, 'attach');
const secret = Buffer.from(vector.fixture_secret_hex, 'hex');
assert.equal(ownerProof(secret, frame.body), vector.proof);
assert.equal(proofMatches(secret, frame.body), true);
for (const field of ['nonce', 'host_instance_id', 'host_release', 'host_sha256', 'executable_sha256', 'sdk_version']) {
  assert.notEqual(ownerProof(secret, { ...frame.body, [field]: `${frame.body[field]}x` }), vector.proof, field);
}
for (const field of Object.keys(frame.body.fence)) {
  assert.notEqual(ownerProof(secret, { ...frame.body, fence: { ...frame.body.fence, [field]: `${frame.body.fence[field]}x` } }), vector.proof, field);
}
for (const field of Object.keys(frame.body.execution)) {
  const value = frame.body.execution[field];
  assert.notEqual(ownerProof(secret, { ...frame.body, execution: { ...frame.body.execution, [field]: typeof value === 'number' ? value + 1 : `${value}x` } }), vector.proof, field);
}
assert.notEqual(ownerProof(secret, { ...frame.body, owner_generation: frame.body.owner_generation + 1 }), vector.proof);
assert.equal(proofMatches(Buffer.alloc(32, 255), frame.body), false);
assert.throws(() => parsePayload(Buffer.from(fixture('attach').toString().replace('"owner_generation":2', '"owner_generation":2.0'))), /Integer token/);
console.log('Shared full-owner HMAC transcript and typed owner generation passed');

const response = JSON.parse(fixture('host-proof'));
const attached = parsePayload(fixture('attached'));
assert.equal(attached.body.type, 'attached');
assert.equal(hostProof(secret, frame.body), response.proof);
assert.equal(attached.body.proof, response.proof);
assert.notEqual(hostProof(secret, frame.body), frame.body.proof, 'request reflection is not a host proof');
assert.notEqual(hostProof(Buffer.alloc(32, 255), frame.body), response.proof);
for (const field of ['nonce', 'host_instance_id', 'host_release', 'host_sha256', 'executable_sha256', 'sdk_version', 'proof']) {
  assert.notEqual(hostProof(secret, { ...frame.body, [field]: `${frame.body[field]}x` }), response.proof, `host ${field}`);
}
assert.notEqual(hostProof(secret, { ...frame.body, owner_generation: frame.body.owner_generation + 1 }), response.proof, 'old host grant replay');
for (const field of Object.keys(frame.body.fence)) {
  assert.notEqual(hostProof(secret, { ...frame.body, fence: { ...frame.body.fence, [field]: `${frame.body.fence[field]}x` } }), response.proof, `host fence ${field}`);
}
for (const field of Object.keys(frame.body.execution)) {
  const value = frame.body.execution[field];
  assert.notEqual(hostProof(secret, { ...frame.body, execution: { ...frame.body.execution, [field]: typeof value === 'number' ? value + 1 : `${value}x` } }), response.proof, `host execution ${field}`);
}
console.log('Direction-separated host proof binds the complete pending Attach and prevents reflection');

const batchVector = JSON.parse(fixture('owner-proof-orchestrator'));
const batch = parsePayload(fixture('attach-orchestrator'));
assert.equal(batch.body.type, 'attach');
assert.equal(batch.body.execution.lane, 'orchestrator');
assert.equal(ownerProof(secret, batch.body), batchVector.proof);
assert.equal(proofMatches(secret, batch.body), true);
assert.notEqual(ownerProof(secret, batch.body), vector.proof);
for (const field of Object.keys(batch.body.execution).filter((field) => field !== 'lane')) {
  const value = batch.body.execution[field];
  assert.notEqual(ownerProof(secret, { ...batch.body, execution: { ...batch.body.execution, [field]: typeof value === 'number' ? value + 1 : `${value}x` } }), batchVector.proof, field);
}
assert.throws(() => parsePayload(Buffer.from(fixture('attach-orchestrator').toString().replace('"lane":"orchestrator"', '"lane":"task"'))));
assert.throws(() => parsePayload(Buffer.from(fixture('attach-orchestrator').toString().replace('"lane":"orchestrator",', '"lane":"orchestrator","task_id":"t",'))));
console.log('An orchestrator batch has its own execution identity and proof, shared with Rust');
