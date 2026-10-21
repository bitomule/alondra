import { expect, mock, test } from 'claude-code/testing'

type Run = { argv: readonly string[]; report: Record<string, unknown> }
type Spawn = { argv: readonly string[]; hello: Record<string, unknown> }

async function settle(): Promise<void> {
  for (let i = 0; i < 2000; i++) await Promise.resolve()
}

test('session.start reports the session and opens the connection to alondrad', async ($, on) => {
  mock.clock(on)
  const runs: Run[] = []
  const spawns: Spawn[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: '/home/runtime/sessions/launch-9f3a.json' }))
  on('process.run', ($, e) => {
    runs.push({ argv: e.argv, report: JSON.parse(e.init.stdin) })
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* ($, e) {
    spawns.push({ argv: e.argv, hello: JSON.parse(e.input) })
    yield { stream: 'stdout', text: 'connected\n' }
    await new Promise(() => {})
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()

  expect(runs.length).toBe(1)
  expect(runs[0].argv.slice(-2)).toEqual(['node', 'event'])
  expect(runs[0].argv[0]).toMatch(/\/bin\/alondra$/)
  expect(runs[0].report).toEqual({
    session_id: 'session-1',
    event: 'session.start',
    version: '2.1.287',
    cwd: '/work',
    launch_key: 'launch-9f3a',
  })
  expect(spawns.length).toBe(1)
  expect(spawns[0].argv.slice(-2)).toEqual(['node', 'connect'])
  expect(spawns[0].hello).toEqual({
    session_id: 'session-1',
    version: '2.1.287',
    cwd: '/work',
    launch_key: 'launch-9f3a',
    working: false,
  })
})

test('a dropped connection is opened again with a growing wait, and never after the session ends', async ($, on) => {
  const clock = mock.clock(on)
  let spawns = 0
  on('session.start', () => ({ cwd: '/work' }))
  on('session.end', ($, e) => ({ sessionId: e.sessionId }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: '', stderr: '' } }))
  on('ui.log', () => ({ value: undefined }))
  on('process.spawn', async function* () {
    spawns += 1
    return { value: { code: 3, signal: null } }
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()
  expect(spawns).toBe(1)
  await clock.advance(999)
  await settle()
  expect(spawns).toBe(1)
  await clock.advance(1)
  await settle()
  expect(spawns).toBe(2)
  await clock.advance(1999)
  await settle()
  expect(spawns).toBe(2)
  await clock.advance(1)
  await settle()
  expect(spawns).toBe(3)

  await $.session.end({ reason: 'other', sessionId: 'session-1', resume: { sessionId: 'session-1' } })
  await clock.advance(60_000)
  await settle()
  expect(spawns).toBe(3)
})

test('a reconnection says whether the session is inside a turn, so a restarted daemon learns it', async ($, on) => {
  const clock = mock.clock(on)
  const hellos: Record<string, unknown>[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: '', stderr: '' } }))
  on('ui.log', () => ({ value: undefined }))
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  on('turn.complete', () => ({ text: '' }))
  on('process.spawn', async function* ($, e) {
    hellos.push(JSON.parse(e.input))
    return { value: { code: 3, signal: null } }
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()
  await $.turn.start({ text: 'go', turnId: 'turn-1' })
  await clock.advance(1000)
  await settle()
  await $.turn.complete({ turnId: 'turn-1', answer: 'Done.', durationMs: 10, isAborted: false, reason: 'answer' })
  await clock.advance(2000)
  await settle()

  expect(hellos.map((hello) => hello.working)).toEqual([false, true, false])
})

test('a helper killed with SIGTERM is not started again', async ($, on) => {
  const clock = mock.clock(on)
  let spawns = 0
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: '', stderr: '' } }))
  on('process.spawn', async function* () {
    spawns += 1
    return { value: { code: null, signal: 'SIGTERM' } }
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await clock.advance(60_000)
  await settle()
  expect(spawns).toBe(1)
})

test('turns and tool calls of the main loop are reported in order, and measurements are not', async ($, on) => {
  mock.clock(on)
  const reports: Record<string, unknown>[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    const { session_id, ...report } = JSON.parse(e.init.stdin)
    expect(session_id).toBe('session-1')
    reports.push(report)
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* () {
    await new Promise(() => {})
  })
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  on('tool.call', () => ({ result: 'ok' }))
  on('turn.complete', () => ({ text: '' }))
  on('session.measure', ($, e) => ({ changed: e.changed }))

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await $.turn.start({ text: 'go', turnId: 'turn-1' })
  await $.tool.call({ tool: 'Bash', tool_use_id: 'tool-1', command: 'cargo test', timeout: 5 })
  await $.tool.call({ tool: 'Read', tool_use_id: 'tool-2', file_path: '/x', agentId: 'sub-1' })
  await $.turn.complete({ turnId: 'sub-turn', answer: 'sub answer', durationMs: 1, isAborted: false, reason: 'answer', agentId: 'sub-1' })
  await $.turn.complete({ turnId: 'turn-1', answer: 'Done.', durationMs: 10, isAborted: true, reason: 'aborted' })
  await $.session.measure({
    context: { tokens: 54944, window: 1000000, percent: 5 },
    rateLimits: [{ kind: 'five_hour', percentUsed: 10, resetsAt: '2026-10-02T16:40:00Z' }],
    cost: { usd: 1.25 },
    changed: ['context', 'rateLimits', 'cost'],
  })
  await settle()

  expect(reports).toEqual([
    { event: 'session.start', version: '2.1.287', cwd: '/work' },
    { event: 'turn.start', turn_id: 'turn-1' },
    { event: 'tool.start', tool: 'Bash', tool_use_id: 'tool-1', input: { command: 'cargo test' } },
    { event: 'tool.end', tool_use_id: 'tool-1' },
    { event: 'subagent.end', id: 'sub-1' },
    { event: 'turn.complete', turn_id: 'turn-1', answer: 'Done.', aborted: true },
  ])
})

test('subagents and background shells are reported when they start, and the list Claude Code gives at the end of a turn replaces them', async ($, on) => {
  mock.clock(on)
  const reports: Record<string, unknown>[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    const { session_id, ...report } = JSON.parse(e.init.stdin)
    expect(session_id).toBe('session-1')
    reports.push(report)
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* () {
    await new Promise(() => {})
  })
  on('agent.spawn', () => ({ model: 'sonnet', agentId: 'agent-1' }))
  on('tool.call', () => ({ result: 'ok' }))
  on('turn.complete', () => ({ text: '' }))
  on('classic.Stop', () => ({}))

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await $.agent.spawn({
    tool_use_id: 'spawn-1',
    prompt: 'Fix it',
    description: 'Fix the parser',
    subagentType: 'general-purpose',
    provider: { plugin: 'engine', tier: 'core' },
    parentModel: 'opus',
    background: true,
    fork: false,
  })
  await $.tool.call({ tool: 'Bash', tool_use_id: 'shell-1', command: 'until false; do sleep 5; done', run_in_background: true })
  await $.tool.call({ tool: 'Bash', tool_use_id: 'shell-2', command: 'ls' })
  await $.turn.complete({ turnId: 'sub-turn', answer: 'Fixed.', durationMs: 1, isAborted: false, reason: 'answer', agentId: 'agent-1', usage: null })
  await $.classic.Stop({
    session_id: 'session-1',
    transcript_path: '/t',
    cwd: '/work',
    hook_event_name: 'Stop',
    stop_hook_active: false,
    background_tasks: [
      { id: 'b1', type: 'shell', status: 'running', description: 'Wait', command: 'until false; do sleep 5; done' },
      { id: 'w1', type: 'workflow', status: 'running', description: 'Review the branch', name: 'review-loop' },
    ],
  })
  await settle()

  expect(reports.filter((report) => !String(report.event).startsWith('tool.'))).toEqual([
    { event: 'session.start', version: '2.1.287', cwd: '/work' },
    { event: 'subagent.start', id: 'agent-1', description: 'Fix the parser', type: 'general-purpose', model: 'sonnet' },
    { event: 'shell.start', id: 'shell-1', command: 'until false; do sleep 5; done' },
    { event: 'subagent.end', id: 'agent-1' },
    {
      event: 'background',
      tasks: [
        { id: 'b1', type: 'shell', description: 'Wait', command: 'until false; do sleep 5; done' },
        { id: 'w1', type: 'workflow', description: 'Review the branch', name: 'review-loop' },
      ],
    },
  ])
})

test('the task list, the todo list and a started workflow are reported from the calls that change them', async ($, on) => {
  mock.clock(on)
  const reports: Record<string, unknown>[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    const { session_id, ...report } = JSON.parse(e.init.stdin)
    reports.push(report)
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* () {
    await new Promise(() => {})
  })
  const results: Record<string, unknown> = {
    TaskCreate: { task: { id: '1', subject: 'Run the tests' } },
    TaskUpdate: { success: true, taskId: '1', updatedFields: ['status'] },
    TaskList: { tasks: [{ id: '1', subject: 'Run the tests', status: 'in_progress', blockedBy: [] }] },
    TodoWrite: { oldTodos: [], newTodos: [] },
    Workflow: { status: 'async_launched', taskId: 'w1', workflowName: 'review-loop', transcriptDir: '/tmp/w1' },
  }
  on('tool.call', ($, e) => ({ result: results[e.tool] ?? 'ok' }))

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await $.tool.call({ tool: 'TaskCreate', tool_use_id: 'c1', subject: 'Run the tests', description: 'All of them', activeForm: 'Running the tests' })
  await $.tool.call({ tool: 'TaskUpdate', tool_use_id: 'c2', taskId: '1', status: 'in_progress', agentId: 'sub-1' } as never)
  await $.tool.call({ tool: 'TaskList', tool_use_id: 'c3' })
  await $.tool.call({
    tool: 'TodoWrite',
    tool_use_id: 'c4',
    todos: [{ content: 'Read', status: 'completed', activeForm: 'Reading' }, { content: 'Fix', status: 'pending', activeForm: 'Fixing' }],
  })
  await $.tool.call({ tool: 'TodoWrite', tool_use_id: 'c5', todos: [], agentId: 'sub-1' } as never)
  await $.tool.call({ tool: 'Workflow', tool_use_id: 'c6', name: 'review-loop' } as never)
  await settle()

  expect(reports.filter((report) => !String(report.event).startsWith('tool.') && report.event !== 'session.start')).toEqual([
    { event: 'step.create', id: '1', subject: 'Run the tests', active_form: 'Running the tests' },
    { event: 'step.update', id: '1', status: 'in_progress' },
    { event: 'steps', steps: [{ id: '1', subject: 'Run the tests', status: 'in_progress' }] },
    {
      event: 'steps',
      steps: [
        { id: '1', subject: 'Read', status: 'completed', active_form: 'Reading' },
        { id: '2', subject: 'Fix', status: 'pending', active_form: 'Fixing' },
      ],
    },
    { event: 'workflow.start', id: 'w1', name: 'review-loop', transcript_dir: '/tmp/w1' },
  ])
})

function pushed(...lines: Record<string, unknown>[]): AsyncGenerator<{ stream: 'stdout'; text: string }> {
  return (async function* () {
    yield { stream: 'stdout' as const, text: 'connected\n' }
    for (const line of lines) yield { stream: 'stdout' as const, text: JSON.stringify(line) + '\n' }
    await new Promise(() => {})
  })()
}

test('a pushed message is submitted to the session and then acknowledged by id', async ($, on) => {
  mock.clock(on)
  const submits: Record<string, unknown>[] = []
  const acks: { argv: readonly string[]; stdin: string }[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    if (e.argv.includes('ack')) acks.push({ argv: e.argv, stdin: e.init.stdin })
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* () {
    yield* pushed(
      { id: 'o-1', text: 'David says hi', as_user: true },
      { id: 'd-2', text: 'A notice', as_user: false },
    )
  })
  on('prompt.submit', ($, e) => {
    submits.push({ text: e.text, asUser: e.origin.asUser === true })
    return { text: e.text }
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()

  expect(submits).toEqual([
    { text: 'David says hi', asUser: true },
    { text: 'A notice', asUser: false },
  ])
  expect(acks.map((ack) => ack.argv.slice(-3))).toEqual([
    ['node', 'ack', 'o-1'],
    ['node', 'ack', 'd-2'],
  ])
  expect(JSON.parse(acks[0].stdin)).toEqual({ session_id: 'session-1' })
})

test('a message waiting for an idle session does not hold up the next one, and a resend is acknowledged without a second submit', async ($, on) => {
  const clock = mock.clock(on)
  let submits = 0
  const acks: string[] = []
  let connections = 0
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    if (e.argv.includes('ack')) acks.push(e.argv[e.argv.length - 1])
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* () {
    connections += 1
    yield { stream: 'stdout' as const, text: 'connected\n{"id":"o-1","text":"first"}\n{"id":"o-2","te' }
    yield { stream: 'stdout' as const, text: 'xt":"second"}\n' }
    if (connections === 1) return { value: { code: 3, signal: null } }
    yield { stream: 'stdout' as const, text: '{"id":"o-2","text":"second"}\n' }
    await new Promise(() => {})
  })
  on('prompt.submit', async ($, e) => {
    submits += 1
    if (e.text === 'first') await new Promise(() => {})
    return { text: e.text }
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()
  expect(submits).toBe(2)
  expect(acks).toEqual(['o-2'])

  await clock.advance(1000)
  await settle()
  expect(submits).toBe(2)
  expect(acks).toEqual(['o-2', 'o-2', 'o-2'])
})

test('an interrupt aborts the running turn by its id, and does nothing between turns', async ($, on) => {
  mock.clock(on)
  const aborts: unknown[] = []
  let release: () => void = () => {}
  const turnStarted = new Promise<void>((resolve) => (release = resolve))
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: '', stderr: '' } }))
  on('process.spawn', async function* () {
    yield { stream: 'stdout' as const, text: 'connected\n' }
    yield { stream: 'stdout' as const, text: '{"interrupt":true}\n' }
    await turnStarted
    yield { stream: 'stdout' as const, text: '{"interrupt":true}\n' }
    await new Promise(() => {})
  })
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  on('turn.abort', ($, e) => {
    aborts.push(e.turnId)
    return {}
  })

  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()
  expect(aborts).toEqual([])
  await $.turn.start({ text: 'go', turnId: 'turn-7' })
  release()
  await settle()
  expect(aborts).toEqual(['turn-7'])
})

type Delivery = { submits: string[]; acks: string[]; push: (line: Record<string, unknown>) => void }

async function startDelivering(
  $: Parameters<Parameters<typeof test>[1]>[0],
  on: Parameters<Parameters<typeof test>[1]>[1],
  tool: (e: { tool: string }) => Record<string, unknown> = () => ({ result: 'ok' }),
): Promise<Delivery> {
  mock.clock(on)
  const delivery: Delivery = { submits: [], acks: [], push: () => {} }
  const lines: string[] = []
  let wake: () => void = () => {}
  delivery.push = (line) => {
    lines.push(JSON.stringify(line) + '\n')
    wake()
  }
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
  on('env.get', () => ({ value: undefined }))
  on('process.run', ($, e) => {
    if (e.argv.includes('ack')) delivery.acks.push(e.argv[e.argv.length - 1])
    return { value: { exitCode: 0, stdout: '', stderr: '' } }
  })
  on('process.spawn', async function* () {
    yield { stream: 'stdout' as const, text: 'connected\n' }
    for (;;) {
      while (lines.length > 0) yield { stream: 'stdout' as const, text: lines.shift()! }
      await new Promise<void>((resolve) => (wake = resolve))
    }
  })
  on('prompt.submit', ($, e) => {
    delivery.submits.push(e.text)
    return { text: e.text }
  })
  on('turn.start', ($, e) => ({ turnId: e.turnId }))
  on('turn.complete', () => ({ text: '' }))
  on('tool.call', ($, e) => tool(e))
  await $.session.start({ surface: 'terminal', isInteractive: true, cwd: '/work' })
  await settle()
  return delivery
}

const ANSWER = '[From David · 11:37] funciona, aunque hay que quitar el onboarding forzado'

test('an answer that arrives while a long build runs reaches the model with that build result and only then counts as read', async ($, on) => {
  let finishBuild: () => void = () => {}
  const build = new Promise<void>((resolve) => (finishBuild = resolve))
  const delivery = await startDelivering($, on, (e) => (e.tool === 'Bash' ? build.then(() => ({ result: 'Build finished' })) : { result: 'ok' }) as never)
  await $.turn.start({ text: 'go', turnId: 'turn-1' })

  const running = $.tool.call({ tool: 'Bash', tool_use_id: 'build', command: 'make device', timeout: 600000 } as never)
  delivery.push({ id: 'o-1369', text: ANSWER, as_user: true })
  await settle()
  expect(delivery.submits).toEqual([])
  expect(delivery.acks).toEqual([])

  finishBuild()
  const out = await running
  await settle()
  expect(out.result).toBe('Build finished')
  expect(out.context).toEqual([`Messages reached you while this tool ran; act on them before you go on:\n\n${ANSWER}`])
  expect(delivery.acks).toEqual(['o-1369'])

  const later = await $.tool.call({ tool: 'Read', tool_use_id: 'read', file_path: '/x' } as never)
  expect(later.context).toBeUndefined()
  await $.turn.complete({ turnId: 'turn-1', answer: 'Done.', durationMs: 1, isAborted: false, reason: 'answer' })
  delivery.push({ id: 'o-1369', text: ANSWER, as_user: true })
  await settle()
  expect(delivery.submits).toEqual([])
  expect(delivery.acks).toEqual(['o-1369', 'o-1369'])
})

test('a tool that fails still carries the message, and a subagent tool never takes it', async ($, on) => {
  const delivery = await startDelivering($, on, (e) => (e.tool === 'Bash' ? { isError: true, result: 'exit 2', text: 'exit 2' } : { result: 'ok' }))
  await $.turn.start({ text: 'go', turnId: 'turn-1' })
  delivery.push({ id: 'd-7', text: ANSWER, as_user: true })
  await settle()

  const subagent = await $.tool.call({ tool: 'Read', tool_use_id: 'sub-read', file_path: '/x', agentId: 'sub-1' } as never)
  expect(subagent.context).toBeUndefined()
  expect(delivery.acks).toEqual([])

  const failed = await $.tool.call({ tool: 'Bash', tool_use_id: 'build', command: 'make', timeout: 5 } as never)
  await settle()
  expect(failed.isError).toBe(true)
  expect(failed.context?.[0]).toContain(ANSWER)
  expect(delivery.acks).toEqual(['d-7'])
})

test('a card asked with an unread answer waiting is refused with the answer, and never reaches Alondra', async ($, on) => {
  const ran: string[] = []
  const delivery = await startDelivering($, on, (e) => {
    ran.push(e.tool)
    return { result: 'Recorded.' }
  })
  await $.turn.start({ text: 'go', turnId: 'turn-1' })
  delivery.push({ id: 'o-1369', text: ANSWER, as_user: true })
  await settle()

  const card = await $.tool.call({ tool: 'mcp__plugin_alondra_alondra__request_test', tool_use_id: 'test-3', interaction_id: 't-3' } as never)
  await settle()
  expect(ran).toEqual([])
  expect(card.deny).toContain('Not sent')
  expect(card.deny).toContain(ANSWER)
  expect(delivery.acks).toEqual(['o-1369'])

  const again = await $.tool.call({ tool: 'mcp__plugin_alondra_alondra__request_test', tool_use_id: 'test-4', interaction_id: 't-4' } as never)
  expect(again).toEqual({ result: 'Recorded.' })
  expect(ran).toEqual(['mcp__plugin_alondra_alondra__request_test'])
})

test('messages still waiting when the turn ends are submitted, and acknowledged once submitted', async ($, on) => {
  const delivery = await startDelivering($, on)
  await $.turn.start({ text: 'go', turnId: 'turn-1' })
  delivery.push({ id: 'o-1', text: ANSWER, as_user: true })
  await settle()
  expect(delivery.submits).toEqual([])

  await $.turn.complete({ turnId: 'turn-1', answer: 'Done.', durationMs: 1, isAborted: false, reason: 'answer' })
  await settle()
  expect(delivery.submits).toEqual([ANSWER])
  expect(delivery.acks).toEqual(['o-1'])
})
