import { expect, mock, test } from 'claude-code/testing'

type Card = { session_id: string; id: string; kind: string; tool?: string; input: Record<string, unknown> }
type Held = { cards: Card[]; runs: () => number }
type Reply = Record<string, unknown> | 'timeout' | 'failure'

const HOLD_RUN_MS = 600_000

async function startWithHolds(
  $: Parameters<Parameters<typeof test>[1]>[0],
  on: Parameters<Parameters<typeof test>[1]>[1],
  answer: (card: Card) => Reply,
  decide: (e: { tool: string; input: { command?: string } }) => Record<string, unknown> = () => ({ decision: 'ask' }),
): Promise<Held> {
  const clock = mock.clock(on)
  on('tool.call', () => ({ result: 'claude code ran the tool' }))
  on('tool.check', (_$: unknown, e: { tool: string; input: { command?: string } }) => decide(e))
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
    if (e.argv.at(-1) !== 'hold') return { value: { exitCode: 0, stdout: '', stderr: '' } }
    runs += 1
    const card = JSON.parse(e.init.stdin) as Card
    cards.push(card)
    const reply = answer(card)
    if (reply === 'timeout') {
      await clock.advance(HOLD_RUN_MS)
      throw new Error('timed out')
    }
    if (reply === 'failure') return { value: { exitCode: 1, stdout: '', stderr: 'no daemon' } }
    return { value: { exitCode: 0, stdout: JSON.stringify(reply), stderr: '' } }
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
  const held = await startWithHolds($, on, () => ({ held: false }))
  const notHeld = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-1', questions } as never)
  const subagent = await $.tool.call({ tool: 'AskUserQuestion', tool_use_id: 'toolu-2', questions, agentId: 'sub-1' } as never)
  expect(notHeld).toEqual({ result: 'claude code ran the tool' })
  expect(subagent).toEqual({ result: 'claude code ran the tool' })
  expect(held.cards.length).toBe(1)
})

test('a plan the user sends back is denied with their words, an approved one is allowed for that call', async ($, on) => {
  const held = await startWithHolds($, on, (card) =>
    card.id === 'toolu-back'
      ? { held: true, approved: false, feedback: 'David requested changes to the plan: use v2' }
      : { held: true, approved: true })

  const back = await $.tool.call({ tool: 'ExitPlanMode', tool_use_id: 'toolu-back', plan: 'p' } as never)
  expect(back).toEqual({ deny: 'David requested changes to the plan: use v2' })

  const approved = await $.tool.call({ tool: 'ExitPlanMode', tool_use_id: 'toolu-ok', plan: 'p' } as never)
  expect(approved).toEqual({ result: 'claude code ran the tool' })
  const checked = await $.tool.check({ tool: 'ExitPlanMode', tool_use_id: 'toolu-ok', input: { plan: 'p' } } as never)
  expect(checked.decision).toBe('allow')
  expect(held.cards.map((card) => card.kind)).toEqual(['plan', 'plan'])
  expect(held.runs()).toBe(2)
})

test('an ask is put to the user and a deny rule is never upgraded', async ($, on) => {
  const held = await startWithHolds($, on, () => ({ held: true, allowed: true, reason: 'David allowed this call' }), (e) =>
    e.input.command?.includes('rm') ? { decision: 'deny', rule: 'Bash(rm *)' } : { decision: 'ask' })

  const allowed = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-1', input: { command: 'make' } } as never)
  expect(allowed.decision).toBe('allow')
  const denied = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-2', input: { command: 'rm -rf /' } } as never)
  expect(denied.decision).toBe('deny')
  expect(held.cards).toEqual([
    { session_id: 'session-1', id: 'toolu-1', kind: 'permission', tool: 'Bash', input: { command: 'make' } },
  ])
})

test('a refused permission becomes a deny with the reason, and an allow rule is not asked about', async ($, on) => {
  const held = await startWithHolds($, on, () => ({ held: true, allowed: false, reason: 'David denied it: not now' }), (e) => ({
    decision: e.tool === 'Read' ? 'allow' : 'ask',
  }))
  const refused = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-1', input: { command: 'make' } } as never)
  expect(refused).toEqual({ decision: 'deny', reason: 'David denied it: not now' })
  const read = await $.tool.check({ tool: 'Read', tool_use_id: 'toolu-2', input: {} } as never)
  expect(read.decision).toBe('allow')
  expect(held.runs()).toBe(1)
})

test('a permission the daemon does not hold stays an ask for Claude Code to put in the terminal', async ($, on) => {
  await startWithHolds($, on, () => ({ held: false }))
  const out = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-1', input: { command: 'make' } } as never)
  expect(out.decision).toBe('ask')
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
  const permission = await $.tool.check({ tool: 'Bash', tool_use_id: 'toolu-2', input: { command: 'make' } } as never)
  expect(permission.decision).toBe('deny')
  expect(held.runs()).toBe(2)
})
