import assert from 'node:assert/strict';
import { ownedHosts } from './owned-host.mjs';
import { HOST_CAPABILITIES } from './persistent.js';

const { wire, startHost, own, lines, cleanup, stderr } = ownedHosts('ct-');
const batch = wire('attach-orchestrator');
const orchestrator = { model: 'opus', tools: ['Read'], disallowed_tools: [], mcp_servers: {}, plugins: [], settings: {} };
const controls = (host) => lines(host.capture + '.controls');
const sdk = (owner, kind, predicate) => owner.sdk(kind, predicate);
const applied = { status: 'accepted', phase: 'applied' };
const input = (n) => `00000000-0000-4000-8000-0000000000${String(n).padStart(2, '0')}`;

try {
  {
    const host = startHost('full');
    const owner = await own(host);
    assert.deepEqual(owner.hello.capabilities, [...HOST_CAPABILITIES]);
    assert.deepEqual(owner.hello.capabilities, ['streaming_input', 'exact_decisions', 'owner_attach', 'commit_ack', 'interrupt', 'set_model', 'set_permission_mode', 'mcp_status']);

    assert.deepEqual((await owner.command({ type: 'mcp_status' })).outcome, { status: 'rejected', code: 'session_absent', message: 'No Claude session is running on this host' }, 'a control before the launch is refused');
    assert.deepEqual(controls(host), [], 'and never reaches Claude');

    const launch = await owner.command({ type: 'start', cwd: host.config.native.cwd, input_id: input(1), prompt: 'first prompt' });
    assert.deepEqual(launch.outcome, { status: 'accepted', phase: 'offered' }, host.stderr());
    await sdk(owner, 'result');

    assert.deepEqual((await owner.command({ type: 'set_model', model: 'unlisted-model' })).outcome, { status: 'rejected', code: 'unsupported_model', message: 'Claude does not offer this model' });
    assert.deepEqual(controls(host), [], 'a model Claude does not list never reaches it');
    assert.deepEqual((await owner.command({ type: 'set_model', model: 'fixture-model' })).outcome, applied, host.stderr());
    assert.deepEqual(controls(host), [{ subtype: 'set_model', model: 'fixture-model' }]);

    assert.deepEqual((await owner.command({ type: 'set_permission_mode', mode: 'dont_ask' })).outcome, applied);
    assert.deepEqual((await owner.command({ type: 'set_permission_mode', mode: 'plan' })).outcome, applied);
    assert.deepEqual((await owner.command({ type: 'set_permission_mode', mode: 'default' })).outcome, applied);
    assert.deepEqual(controls(host).slice(1), ['dontAsk', 'plan', 'default'].map((mode) => ({ subtype: 'set_permission_mode', mode })));

    const status = await owner.command({ type: 'mcp_status' });
    assert.deepEqual(status.outcome, applied);
    const result = await sdk(owner, 'control_result');
    assert.deepEqual(result.body.observation, { type: 'sdk', kind: 'control_result', native: {}, payload: { control: 'mcp_status', command_id: status.id, servers: [{ name: 'fixture', status: 'connected' }] }, failures: [] });
    assert(owner.frames.indexOf(result) < status.at, 'the result is evidence before the receipt that closes the command');
    assert.deepEqual(controls(host).at(-1), { subtype: 'mcp_status' });

    const held = await owner.command({ type: 'offer', input_id: input(2), prompt: 'consume-and-hold' });
    assert.deepEqual(held.outcome, { status: 'accepted', phase: 'offered' });
    assert.deepEqual((await owner.command({ type: 'interrupt' })).outcome, applied);
    assert.deepEqual(controls(host).at(-1), { subtype: 'interrupt' });
    await sdk(owner, 'result', (payload) => payload.subtype === 'error_during_execution' && payload.user_message_uuid === input(2));

    const compact = await owner.command({ type: 'offer', input_id: input(3), prompt: '/compact' });
    assert.deepEqual(compact.outcome, { status: 'accepted', phase: 'offered' });
    await sdk(owner, 'status', (payload) => payload.subtype === 'compact_boundary');
    await sdk(owner, 'result', (payload) => payload.subtype === 'success' && payload.user_message_uuids?.includes(input(3)));
    assert.equal(lines(host.capture + '.prompts').at(-1), '/compact', 'compaction is an ordinary input');
  }
  {
    const declared = HOST_CAPABILITIES.filter((capability) => capability !== 'set_model' && capability !== 'set_permission_mode');
    const host = startHost('narrow', { capabilities: declared });
    const owner = await own(host);
    assert.deepEqual(owner.hello.capabilities, declared, 'the hello names only what this host does');
    assert.deepEqual((await owner.command({ type: 'start', cwd: host.config.native.cwd, input_id: input(1), prompt: 'first prompt' })).outcome, { status: 'accepted', phase: 'offered' });
    await sdk(owner, 'result');
    assert.deepEqual((await owner.command({ type: 'set_model', model: 'fixture-model' })).outcome, { status: 'rejected', code: 'unsupported', message: 'This host does not support set_model' });
    assert.deepEqual((await owner.command({ type: 'set_permission_mode', mode: 'plan' })).outcome, { status: 'rejected', code: 'unsupported', message: 'This host does not support set_permission_mode' });
    assert.deepEqual((await owner.command({ type: 'mcp_status' })).outcome, applied, 'the declared controls still work');
    assert.deepEqual(controls(host), [{ subtype: 'mcp_status' }], 'an undeclared control never reaches Claude');
  }
  {
    const host = startHost('batch', { fence: batch.fence, execution: batch.execution, profile: orchestrator });
    const owner = await own(host);
    const single = { status: 'rejected', code: 'single_turn', message: 'An orchestrator batch is one turn: a later input waits for the next batch' };
    for (const action of [{ type: 'set_model', model: 'fixture-model' }, { type: 'set_permission_mode', mode: 'plan' }, { type: 'mcp_status' }, { type: 'respond', request_id: 'decision-1', epoch: 1, kind: 'permission', tool_use_id: 'toolu_fixture_1', decision: { behavior: 'deny', message: 'No' }, asked: { tool: 'Bash', input: { command: 'make' } } }]) {
      assert.deepEqual((await owner.command(action)).outcome, single, action.type);
    }
    assert.deepEqual(controls(host), []);
  }
  console.log('Host controls: each declared control reaches Claude with its receipt; refusals have no effect');
} catch (error) {
  console.error(error, stderr());
  process.exitCode = 1;
} finally {
  await cleanup();
}
