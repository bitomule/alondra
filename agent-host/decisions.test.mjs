import assert from 'node:assert/strict';
import { ownedHosts } from './owned-host.mjs';
import { HOST_CAPABILITIES } from './persistent.js';

const { startHost, own, lines, cleanup, stderr } = ownedHosts('dt-');
const applied = { status: 'accepted', phase: 'applied' };
const offered = { status: 'accepted', phase: 'offered' };
const stale = { status: 'rejected', code: 'stale_request', message: 'Claude is not waiting for this request' };
const input = (n) => `00000000-0000-4000-8000-0000000001${String(n).padStart(2, '0')}`;
const answered = (host) => lines(host.capture + '.decisions');

async function session(name, options) {
  const host = startHost(name, options);
  const owner = await own(host);
  let inputs = 0;
  owner.prompt = async (prompt) => {
    const id = input(++inputs);
    assert.deepEqual((await owner.command(inputs === 1 ? { type: 'start', cwd: host.config.native.cwd, input_id: id, prompt } : { type: 'offer', input_id: id, prompt })).outcome, offered, host.stderr());
    return id;
  };
  const seen = new Set();
  owner.ask = async (prompt) => {
    const id = await owner.prompt(prompt);
    const frame = await owner.event('decision_requested', (_, candidate) => !seen.has(candidate.body.cursor));
    seen.add(frame.body.cursor);
    const { request_id, kind, tool_use_id, payload } = frame.body.observation;
    return { input: id, payload, frame, identity: { request_id, epoch: frame.body.cursor, kind, tool_use_id: tool_use_id ?? null, asked: payload } };
  };
  owner.settled = (epoch) => owner.event('decision_settled', (observation) => observation.epoch === epoch);
  owner.finished = (id) => owner.sdk('result', (payload) => payload.user_message_uuid === id);
  owner.respond = (identity, decision) => owner.command({ type: 'respond', ...identity, decision });
  await owner.prompt('first prompt');
  await owner.sdk('result');
  return { host, owner };
}

try {
  {
    const { host, owner } = await session('full');
    assert(owner.hello.capabilities.includes('exact_decisions'));
    assert.deepEqual((await owner.respond({ request_id: 'decision-1', epoch: 1, kind: 'permission', tool_use_id: 'toolu_fixture_1', asked: {} }, { behavior: 'allow', updated_input: {} })).outcome, stale, 'nothing pending: an answer reaches nothing');

    const bash = await owner.ask('decide:bash git status');
    assert.deepEqual([bash.identity.request_id, bash.identity.kind, bash.identity.tool_use_id], ['decision-1', 'permission', 'toolu_fixture_1']);
    assert.equal(bash.payload.tool, 'Bash');
    assert.deepEqual(bash.payload.input, { command: 'git status' });
    assert.equal(bash.payload.request.toolUseID, 'toolu_fixture_1');
    for (const [why, identity, decision] of [
      ['an old epoch', { ...bash.identity, epoch: bash.identity.epoch - 1 }, { behavior: 'allow', updated_input: { command: 'git status' } }],
      ['a later epoch', { ...bash.identity, epoch: bash.identity.epoch + 1 }, { behavior: 'allow', updated_input: { command: 'git status' } }],
      ['another kind', { ...bash.identity, kind: 'plan' }, { behavior: 'allow', updated_input: { command: 'git status' } }],
      ['another tool use', { ...bash.identity, tool_use_id: 'toolu_other' }, { behavior: 'allow', updated_input: { command: 'git status' } }],
      ['an unknown request', { ...bash.identity, request_id: 'decision-9' }, { behavior: 'allow', updated_input: { command: 'git status' } }],
      ['an elicitation answer', { ...bash.identity, kind: 'elicitation', tool_use_id: null }, { behavior: 'accept' }],
      ['the answer to another command', { ...bash.identity, asked: { ...bash.payload, input: { command: 'rm -rf build' } } }, { behavior: 'allow', updated_input: { command: 'git status' } }],
      ['a denial of another command', { ...bash.identity, asked: { ...bash.payload, input: { command: 'rm -rf build' } } }, { behavior: 'deny', message: 'No' }],
      ['an answer that names no request', { ...bash.identity, asked: {} }, { behavior: 'allow', updated_input: { command: 'git status' } }],
    ]) assert.deepEqual((await owner.respond(identity, decision)).outcome, stale, why);
    assert.deepEqual(answered(host), [], 'a refused answer never reaches Claude');
    assert(!owner.frames.some((frame) => frame.body.type === 'event' && frame.body.observation.type === 'decision_settled'), 'and settles nothing');

    const allow = await owner.respond(bash.identity, { behavior: 'allow', updated_input: { command: 'git status' } });
    assert.deepEqual(allow.outcome, applied, host.stderr());
    const settled = await owner.settled(bash.identity.epoch);
    assert.deepEqual(settled.body.observation, { type: 'decision_settled', request_id: 'decision-1', epoch: bash.identity.epoch, outcome: 'answered' });
    assert(owner.frames.indexOf(settled) < allow.at, 'the settlement is evidence before the receipt');
    await owner.finished(bash.input);
    assert.deepEqual(answered(host), [{ request_id: 'decision-1', subtype: 'can_use_tool', cancelled: false, outcome: 'success', response: { behavior: 'allow', updatedInput: { command: 'git status' }, toolUseID: 'toolu_fixture_1' }, tool_name: 'Bash', tool_use_id: 'toolu_fixture_1' }]);
    assert.deepEqual((await owner.respond(bash.identity, { behavior: 'deny', message: 'Too late' })).outcome, stale, 'an answered request takes no second answer');

    const question = await owner.ask('decide:questions');
    assert.equal(question.identity.kind, 'question');
    assert.deepEqual(question.payload.input.questions.map((entry) => entry.question), ['Which fixture color?', 'Which fixture sizes?']);
    const answers = { questions: question.payload.input.questions, answers: { 'Which fixture color?': 'Red', 'Which fixture sizes?': 'Small, Large' } };
    assert.deepEqual((await owner.respond(question.identity, { behavior: 'allow', updated_input: answers })).outcome, applied);
    await owner.finished(question.input);
    assert.deepEqual(answered(host).at(-1).response.updatedInput, answers);

    const single = await owner.ask('decide:question');
    assert.equal(single.payload.input.questions.length, 1);
    assert.deepEqual((await owner.respond(single.identity, { behavior: 'deny', message: 'The user denied it' })).outcome, applied);
    await owner.finished(single.input);
    assert.deepEqual(answered(host).at(-1).response, { behavior: 'deny', message: 'The user denied it', toolUseID: single.identity.tool_use_id });

    const plan = await owner.ask('decide:plan');
    assert.equal(plan.identity.kind, 'plan');
    assert.match(plan.payload.input.plan, /- \[ \] Change the fixture/);
    assert.deepEqual((await owner.respond(plan.identity, { behavior: 'allow', updated_input: plan.payload.input })).outcome, applied);
    await owner.finished(plan.input);
    assert.equal(answered(host).at(-1).response.behavior, 'allow');
    assert.deepEqual(answered(host).at(-1).response.updatedPermissions, [{ type: 'setMode', mode: 'auto', destination: 'session' }],
      'an approved plan returns the node to auto mode, never to default');

    const changes = await owner.ask('decide:plan');
    assert.deepEqual((await owner.respond(changes.identity, { behavior: 'deny', message: 'The user requests changes to the plan' })).outcome, applied);
    await owner.finished(changes.input);
    assert.equal(answered(host).at(-1).response.updatedPermissions, undefined, 'a plan sent back stays in plan mode');

    for (const [prompt, source] of [['decide:alondra', 'plugin'], ['decide:imitation', 'user']]) {
      const tool = await owner.ask(prompt);
      assert.equal(tool.identity.kind, 'permission');
      assert.equal(tool.payload.tool, 'mcp__plugin_alondra_alondra__ask');
      assert.deepEqual(tool.payload.request.mcpServer, { name: 'plugin:alondra:alondra', source });
      assert.deepEqual((await owner.respond(tool.identity, { behavior: 'deny', message: 'Not now' })).outcome, applied);
      await owner.finished(tool.input);
    }

    const accept = await owner.ask('decide:elicitation');
    assert.deepEqual(accept.identity, { request_id: accept.identity.request_id, epoch: accept.identity.epoch, kind: 'elicitation', tool_use_id: null, asked: accept.payload });
    assert.equal('tool_use_id' in accept.frame.body.observation, false, 'an elicitation names no tool use');
    assert.deepEqual(accept.payload, { serverName: 'fixture-server', message: 'Which fixture name?', mode: 'form', requestedSchema: { type: 'object', properties: { name: { type: 'string', title: 'Name' } }, required: ['name'] } });
    assert.deepEqual((await owner.respond({ ...accept.identity, kind: 'permission', tool_use_id: 'toolu_fixture_1' }, { behavior: 'allow', updated_input: {} })).outcome, stale, 'a permission answer never answers an elicitation');
    assert.deepEqual((await owner.respond(accept.identity, { behavior: 'accept', content: { name: 'Ada' } })).outcome, applied);
    await owner.finished(accept.input);
    assert.deepEqual(answered(host).at(-1), { request_id: accept.identity.request_id, subtype: 'elicitation', cancelled: false, outcome: 'success', response: { action: 'accept', content: { name: 'Ada' } } });
    const decline = await owner.ask('decide:elicitation');
    assert.deepEqual((await owner.respond(decline.identity, { behavior: 'decline' })).outcome, applied);
    await owner.finished(decline.input);
    assert.deepEqual(answered(host).at(-1).response, { action: 'decline' });

    for (const prompt of ['decide:bash rm -rf build', 'decide:elicitation']) {
      const interrupted = await owner.ask(prompt);
      const before = answered(host).length;
      assert.deepEqual((await owner.command({ type: 'interrupt' })).outcome, applied);
      const cancelled = await owner.settled(interrupted.identity.epoch);
      assert.equal(cancelled.body.observation.outcome, 'cancelled', prompt);
      await owner.sdk('result', (payload) => payload.subtype === 'error_during_execution' && payload.user_message_uuid === interrupted.input);
      assert.deepEqual((await owner.respond(interrupted.identity, interrupted.identity.kind === 'elicitation' ? { behavior: 'accept', content: { name: 'Late' } } : { behavior: 'allow', updated_input: { command: 'rm -rf build' } })).outcome, stale, 'an aborted request takes no answer');
      const after = await owner.prompt('after the interruption');
      await owner.finished(after);
      const reached = answered(host).slice(before);
      assert.equal(reached.length, 1, `${prompt}: Claude got exactly the cancellation`);
      assert.equal(reached[0].cancelled, true);
      assert.deepEqual(reached[0].response, interrupted.identity.kind === 'elicitation' ? { action: 'cancel' } : { behavior: 'deny', message: 'Permission request cancelled', toolUseID: interrupted.identity.tool_use_id });
    }

    const closing = await owner.ask('decide:bash make release');
    assert.deepEqual((await owner.command({ type: 'close' })).outcome, applied);
    assert.equal((await owner.settled(closing.identity.epoch)).body.observation.outcome, 'cancelled', 'a session that ends denies what it still waited on');

    const requested = owner.frames.filter((frame) => frame.body.type === 'event' && frame.body.observation.type === 'decision_requested').map((frame) => frame.body.cursor);
    const settlements = owner.frames.filter((frame) => frame.body.type === 'event' && frame.body.observation.type === 'decision_settled').map((frame) => frame.body.observation.epoch);
    assert.deepEqual(settlements, requested, 'exactly one settlement per request, in order');
  }
  {
    const declared = HOST_CAPABILITIES.filter((capability) => capability !== 'exact_decisions');
    const { host, owner } = await session('undeclared', { capabilities: declared });
    assert.deepEqual(owner.hello.capabilities, declared);
    const bash = await owner.ask('decide:bash git status');
    assert.deepEqual((await owner.respond(bash.identity, { behavior: 'allow', updated_input: { command: 'git status' } })).outcome, { status: 'rejected', code: 'unsupported', message: 'This host does not support respond' });
    assert.deepEqual((await owner.command({ type: 'mcp_status' })).outcome, applied, 'a later command proves the refusal was already processed');
    assert.deepEqual(answered(host), [], 'a host that does not declare decisions never answers Claude');
    assert(!owner.frames.some((frame) => frame.body.type === 'event' && frame.body.observation.type === 'decision_settled'));
  }
  console.log('Host decisions: only the pending request with its epoch, kind and tool use is answered; every request settles once');
} catch (error) {
  console.error(error, stderr());
  process.exitCode = 1;
} finally {
  await cleanup();
}
