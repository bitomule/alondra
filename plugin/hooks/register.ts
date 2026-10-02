import type { EngineInterface, ProcessSpawnResult, Register } from 'claude-code'

const REPORT_TIMEOUT_MS = 10_000
const FIRST_RETRY_MS = 1_000
const MAX_RETRY_MS = 30_000
const ANSWER_LIMIT = 8_000
const SUMMARY_FIELDS = ['command', 'file_path', 'description', 'pattern', 'url'] as const
const SUMMARY_LIMIT = 300

type Session = { id: string; alondra: string; hello: string }

let session: Session | undefined
let stopping = false
let retryMs = FIRST_RETRY_MS
let reports: Promise<void> = Promise.resolve()

async function send($: EngineInterface, report: Record<string, unknown>): Promise<void> {
  if (!session) return
  try {
    const sent = await $.process.run([session.alondra, 'node', 'event'], {
      stdin: JSON.stringify({ session_id: session.id, ...report }),
      timeoutMs: REPORT_TIMEOUT_MS,
    })
    if (sent.exitCode !== 0) $.ui.log(`Alondra did not take ${report.event}: ${sent.stderr.trim()}`, { to: 'debug' })
  } catch (error) {
    $.ui.log(`Alondra did not take ${report.event}: ${error}`, { to: 'debug' })
  }
}

function queue($: EngineInterface, report: Record<string, unknown>): void {
  reports = reports.then(() => send($, report))
}

function summaryInput(call: Record<string, unknown>): Record<string, unknown> {
  const input: Record<string, unknown> = {}
  for (const field of SUMMARY_FIELDS) {
    const value = call[field]
    if (typeof value === 'string') input[field] = value.slice(0, SUMMARY_LIMIT)
  }
  const questions = call.questions
  if (Array.isArray(questions) && typeof questions[0]?.question === 'string') {
    input.questions = [{ question: questions[0].question.slice(0, SUMMARY_LIMIT) }]
  }
  return input
}

async function connect($: EngineInterface): Promise<void> {
  if (!session || stopping) return
  let ended: ProcessSpawnResult = { code: null, signal: null }
  try {
    const helper = $.process.spawn({ argv: [session.alondra, 'node', 'connect'], input: session.hello })
    for (;;) {
      const step = await helper.next()
      if (step.done) {
        ended = step.value ?? ended
        break
      }
      if (step.value.stream === 'stdout' && step.value.text.includes('connected')) retryMs = FIRST_RETRY_MS
    }
  } catch (error) {
    $.ui.log(`Alondra connection failed: ${error}`, { to: 'debug' })
  }
  if (stopping || ended.signal === 'SIGTERM') return
  const wait = retryMs
  retryMs = Math.min(retryMs * 2, MAX_RETRY_MS)
  $.clock.after(wait, () => {
    void connect($)
  })
}

export const register: Register = (on) => {
  on('session.start', async ($, e, next) => {
    const started = await next(e)
    const { version } = await $.session.version()
    const id = await $.session.id()
    stopping = false
    retryMs = FIRST_RETRY_MS
    session = {
      id,
      alondra: $.plugin.root + '/bin/alondra',
      hello: JSON.stringify({ session_id: id, version, cwd: e.cwd }),
    }
    queue($, { event: 'session.start', version, cwd: e.cwd })
    void connect($)
    return started
  })

  on('session.end', async ($, e, next) => {
    stopping = true
    queue($, { event: 'session.end', reason: e.reason })
    return next(e)
  })

  on('session.measure', async ($, e, next) => {
    queue($, {
      event: 'session.measure',
      context: e.context,
      rate_limits: e.rateLimits.map((limit) => ({
        kind: limit.kind,
        percent_used: limit.percentUsed,
        resets_at: limit.resetsAt,
      })),
      cost_usd: e.cost?.usd,
    })
    return next(e)
  })

  on('turn.start', async ($, e, next) => {
    queue($, { event: 'turn.start', turn_id: e.turnId })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (e.agentId === undefined) {
      queue($, {
        event: 'turn.complete',
        turn_id: e.turnId,
        answer: e.answer.slice(0, ANSWER_LIMIT),
        aborted: e.isAborted,
      })
    }
    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    queue($, { event: 'tool.start', tool: e.tool, tool_use_id: e.tool_use_id, input: summaryInput(e) })
    try {
      return await next(e)
    } finally {
      queue($, { event: 'tool.end', tool_use_id: e.tool_use_id })
    }
  })
}
