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

test('turns, tool calls and measurements of the main loop are reported in order', async ($, on) => {
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
    { event: 'turn.complete', turn_id: 'turn-1', answer: 'Done.', aborted: true },
    {
      event: 'session.measure',
      context: { tokens: 54944, window: 1000000, percent: 5 },
      rate_limits: [{ kind: 'five_hour', percent_used: 10, resets_at: '2026-10-02T16:40:00Z' }],
      cost_usd: 1.25,
    },
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
