import { expect, mock, test } from 'claude-code/testing'

type Run = { argv: readonly string[]; report: Record<string, unknown> }
type Spawn = { argv: readonly string[]; hello: Record<string, unknown> }

async function settle(): Promise<void> {
  for (let i = 0; i < 100; i++) await Promise.resolve()
}

test('session.start reports the session and opens the connection to alondrad', async ($, on) => {
  mock.clock(on)
  const runs: Run[] = []
  const spawns: Spawn[] = []
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
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
  expect(runs[0].report).toEqual({ session_id: 'session-1', event: 'session.start', version: '2.1.287', cwd: '/work' })
  expect(spawns.length).toBe(1)
  expect(spawns[0].argv.slice(-2)).toEqual(['node', 'connect'])
  expect(spawns[0].hello).toEqual({ session_id: 'session-1', version: '2.1.287', cwd: '/work' })
})

test('a dropped connection is opened again with a growing wait, and never after the session ends', async ($, on) => {
  const clock = mock.clock(on)
  let spawns = 0
  on('session.start', () => ({ cwd: '/work' }))
  on('session.end', ($, e) => ({ sessionId: e.sessionId }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
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

test('a helper killed with SIGTERM is not started again', async ($, on) => {
  const clock = mock.clock(on)
  let spawns = 0
  on('session.start', () => ({ cwd: '/work' }))
  on('session.id', () => ({ value: 'session-1' }))
  on('session.version', () => ({ value: { version: '2.1.287' } }))
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
