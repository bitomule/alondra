import type { Args, ProcessRunResult, ResultOf } from 'claude-code'
import { expect, mock, test, type TestBody } from 'claude-code/testing'

function processResult(stdout = '', exitCode = 0, stderr = ''): ProcessRunResult {
  return { exitCode, stdout, stderr, isStdoutTruncated: false, isStderrTruncated: false }
}

function required<T>(value: T | undefined): T {
  if (value === undefined) throw new Error('Expected a test fixture value')
  return value
}

type Card = { session_id: string; id: string; kind: string; tool?: string; cwd?: string; input: Record<string, unknown> }
type Held = { cards: Card[]; runs: () => number }
type Reply = Record<string, unknown> | 'timeout' | 'failure'

const HOLD_RUN_MS = 600_000

async function startWithHolds(
  $: Parameters<TestBody>[0],
  on: Parameters<TestBody>[1],
  answer: (card: Card) => Reply,
  decide: (e: Args<'tool.check'>) => ResultOf['tool.check'] = () => ({ decision: 'ask' }),
): Promise<Held> {
  const clock = mock.clock(on)
  on('tool.call', () => ({ result: 'claude code ran the tool' }))
  on('tool.check', (_$, e) => decide(e))
  const cards: Card[] = []
  let runs = 0
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.spawn', async function* () {
    await new Promise(() => {})
  })
  on('process.run', async ($, e) => {
    if (e.argv.at(-1) !== 'hold') return { value: processResult() }
    runs += 1
    const card = JSON.parse(required(e.init?.stdin)) as Card
    cards.push(card)
    const reply = answer(card)
    if (reply === 'timeout') {
      await clock.advance(HOLD_RUN_MS)
      throw new Error('timed out')
    }
    if (reply === 'failure') return { value: processResult('', 1, 'no daemon') }
    return { value: processResult(JSON.stringify(reply)) }
  })
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  return { cards, runs: () => runs }
}

const questions = [{ question: 'Which colour?', header: 'Colour', options: [{ label: 'Red' }, { label: 'Blue' }], multiSelect: false }]

test('AskUserQuestion is answered from the card and never reaches the tool', async ($, on) => {
  const held = await startWithHolds($, on, () => ({ held: true, answers: { 'Which colour?': 'Blue' } }))
  const out = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-1', questions } as never)
  expect(out).toEqual({ result: { questions, answers: { 'Which colour?': 'Blue' } } })
  expect(held.cards).toEqual([{ session_id: 'session-1', id: 'toolu-1', kind: 'question', input: { questions } }])
})

test('a question the daemon does not hold, or a subagent asks, goes to Claude Code', async ($, on) => {
  const held = await startWithHolds($, on, () => ({ held: false }), () => ({ decision: 'allow' }))
  const notHeld = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-1', questions } as never)
  const subagent = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-2', questions, agentId: 'sub-1' } as never)
  expect(notHeld).toEqual({ result: 'claude code ran the tool' })
  expect(subagent).toEqual({ result: 'claude code ran the tool' })
  expect(held.cards.length).toBe(1)
})

test('a plan the user sends back is denied with their words, an approved one is allowed for that call', async ($, on) => {
  const asked: string[] = []
  const held = await startWithHolds($, on, (card) =>
    card.id === 'toolu-back'
      ? { held: true, approved: false, feedback: 'Alice requested changes to the plan: use v2' }
      : { held: true, approved: true }, (e) => {
    asked.push(e.tool)
    return { decision: 'ask' }
  })

  const back = await $.tool.call({ tool: 'ExitPlanMode', tool_use_id: 'toolu-back', plan: 'p' } as never)
  expect(back).toEqual({ deny: 'Alice requested changes to the plan: use v2' })

  const approved = await $.tool.call({ tool: 'ExitPlanMode', tool_use_id: 'toolu-ok', plan: 'p' } as never)
  expect(approved).toEqual({ result: 'claude code ran the tool' })
  expect(asked).toEqual([])
  expect(held.cards.map((card) => card.kind)).toEqual(['plan', 'plan'])
  expect(held.cards.map((card) => card.cwd)).toEqual(['/work', '/work'])
  expect(held.cards.map((card) => card.input)).toEqual([{ plan: 'p' }, { plan: 'p' }])
  expect(held.runs()).toBe(2)
})

test('permission queries without a call id use the rules, and plans can omit their text', async ($, on) => {
  const asked: (string | undefined)[] = []
  const held = await startWithHolds($, on, () => ({ held: true, approved: true }), (e) => {
    asked.push(e.tool_use_id)
    return { decision: 'ask' }
  })

  const query = await $.tool.check({ tool: 'ExitPlanMode', input: {} })
  expect(query.decision).toBe('ask')
  const call = { tool: 'ExitPlanMode' as const, tool_use_id: 'plan-without-text' }
  expect(await $.tool.call(call)).toEqual({ result: 'claude code ran the tool' })
  expect(held.cards.map((card) => card.input)).toEqual([{}])
  expect(asked).toEqual([undefined])
  expect((await $.tool.check({ tool: 'ExitPlanMode', input: {} })).decision).toBe('ask')
  expect(asked).toEqual([undefined, undefined])
})

test('an ask from the rules is left for Claude Code, so auto mode can still approve it and nothing is held', async ($, on) => {
  const held = await startWithHolds($, on, () => ({ held: true, allowed: true }), (e) =>
    typeof e.input === 'object' && e.input !== null && 'command' in e.input && typeof e.input.command === 'string' && e.input.command.includes('rm') ? { decision: 'deny', rule: 'Bash(rm *)' } : { decision: 'ask' })

  const asked = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-1', input: { command: 'grep x log | tail -20; grep -c "" log' } })
  expect(asked.decision).toBe('ask')
  const denied = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-2', input: { command: 'rm -rf /' } })
  expect(denied.decision).toBe('deny')
  expect(held.runs()).toBe(0)
})

test('a cut wait is run again with the same card until the user answers', async ($, on) => {
  let asked = 0
  const held = await startWithHolds($, on, () => (++asked < 3 ? 'timeout' : { held: true, answers: { 'Which colour?': 'Red' } }))
  const out = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-1', questions } as never)
  expect(out).toEqual({ result: { questions, answers: { 'Which colour?': 'Red' } } })
  expect(held.cards.map((card) => card.id)).toEqual(['toolu-1', 'toolu-1', 'toolu-1'])
})

test('a helper that fails at once denies the call instead of letting it through', async ($, on) => {
  const held = await startWithHolds($, on, () => 'failure')
  const question = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-1', questions } as never)
  expect(question).toHaveProperty('deny')
  expect(held.runs()).toBe(1)
})

test('under the Agent SDK a question and a plan go to Claude Code, which asks the host, and nothing is held', async ($, on) => {
  on('tool.call', () => ({ result: 'claude code ran the tool' }))
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.300' } }))
  on('env.get', () => ({ value: '1' }))
  let spawns = 0
  on('process.spawn', async function* () {
    spawns += 1
    await new Promise(() => {})
  })
  const runs: (readonly string[])[] = []
  on('process.run', async (_$, e) => {
    runs.push(e.argv)
    return { value: processResult(JSON.stringify({ held: true, answers: { 'Which colour?': 'Blue' } })) }
  })
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  const question = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-1', questions } as never)
  const plan = await $.tool.call({ tool: 'ExitPlanMode', tool_use_id: 'toolu-2', plan: 'p' } as never)
  expect(question).toEqual({ result: 'claude code ran the tool' })
  expect(plan).toEqual({ result: 'claude code ran the tool' })
  expect(runs).toEqual([])
  expect(spawns).toBe(0)
})
