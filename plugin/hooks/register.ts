import type { EngineInterface, ProcessSpawnResult, Register } from 'claude-code'

const REPORT_TIMEOUT_MS = 10_000
const FIRST_RETRY_MS = 1_000
const MAX_RETRY_MS = 30_000
const ANSWER_LIMIT = 8_000
const SUMMARY_FIELDS = ['command', 'file_path', 'description', 'pattern', 'url'] as const
const SUMMARY_LIMIT = 300

const HOLD_RUN_MS = 600_000

type Session = { id: string; alondra: string; version: string; cwd: string; launchKey?: string }

type Card = { id: string; kind: 'question' | 'plan' | 'permission'; tool?: string; input: Record<string, unknown> }
type Answer = {
  held: boolean
  answers?: Record<string, string>
  approved?: boolean
  feedback?: string
  allowed?: boolean
  reason?: string
}

const approvedPlans = new Set<string>()

let session: Session | undefined
let stopping = false
let retryMs = FIRST_RETRY_MS
let reports: Promise<void> = Promise.resolve()
let turnId: string | undefined
const submitting = new Set<string>()
const submitted = new Set<string>()

type Push = { id?: string; text?: string; as_user?: boolean; interrupt?: boolean }

function hello(current: Session): string {
  return JSON.stringify({
    session_id: current.id,
    version: current.version,
    cwd: current.cwd,
    launch_key: current.launchKey,
    working: turnId !== undefined,
  })
}

async function launchKey($: EngineInterface): Promise<string | undefined> {
  const settings = await $.env.get('ALONDRA_DOMAIN_SETTINGS')
  const file = settings?.split('/').pop()
  return file ? file.replace(/\.json$/, '') : undefined
}

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

async function acknowledge($: EngineInterface, id: string): Promise<void> {
  if (!session) return
  try {
    const sent = await $.process.run([session.alondra, 'node', 'ack', id], {
      stdin: JSON.stringify({ session_id: session.id }),
      timeoutMs: REPORT_TIMEOUT_MS,
    })
    if (sent.exitCode !== 0) $.ui.log(`Alondra did not take the ack of ${id}: ${sent.stderr.trim()}`, { to: 'debug' })
  } catch (error) {
    $.ui.log(`Alondra did not take the ack of ${id}: ${error}`, { to: 'debug' })
  }
}

async function submit($: EngineInterface, id: string, text: string, asUser: boolean): Promise<void> {
  if (submitted.has(id)) return acknowledge($, id)
  if (submitting.has(id)) return
  submitting.add(id)
  try {
    await $.prompt.submit({ text, asUser })
    submitted.add(id)
  } catch (error) {
    $.ui.log(`Alondra message ${id} was not submitted: ${error}`, { to: 'debug' })
    return
  } finally {
    submitting.delete(id)
  }
  await acknowledge($, id)
}

async function receive($: EngineInterface, line: string): Promise<void> {
  let push: Push
  try {
    push = JSON.parse(line)
  } catch {
    $.ui.log(`Alondra sent a line that is not JSON: ${line.slice(0, 200)}`, { to: 'debug' })
    return
  }
  if (push.interrupt) {
    if (turnId !== undefined) await $.turn.abort({ turnId })
  } else if (typeof push.id === 'string' && typeof push.text === 'string') {
    void submit($, push.id, push.text, push.as_user === true)
  }
}

// The wait lives inside `$.process.run`, which the hook's 10 s budget does not count. Claude Code
// ends a run at its timeout, so the helper is run again and posts the same card id: alondrad finds
// the card already open, which is also how a card outlives a daemon restart.
async function hold($: EngineInterface, next: { signal: AbortSignal }, card: Card): Promise<Answer> {
  if (!session) return { held: false }
  for (;;) {
    const startedAt = await $.clock.now()
    try {
      const run = await $.process.run([session.alondra, 'node', 'hold'], {
        stdin: JSON.stringify({ session_id: session.id, ...card }),
        timeoutMs: HOLD_RUN_MS,
      })
      if (run.exitCode !== 0) throw new Error(`alondra node hold exited ${run.exitCode}: ${run.stderr.trim()}`)
      return JSON.parse(run.stdout) as Answer
    } catch (error) {
      const ranOut = (await $.clock.now()) - startedAt >= HOLD_RUN_MS - 2_000
      if (!ranOut || next.signal.aborted) throw error
    }
  }
}

async function connect($: EngineInterface): Promise<void> {
  if (!session || stopping) return
  let ended: ProcessSpawnResult = { code: null, signal: null }
  let pending = ''
  try {
    const helper = $.process.spawn({ argv: [session.alondra, 'node', 'connect'], input: hello(session) })
    for (;;) {
      const step = await helper.next()
      if (step.done) {
        ended = step.value ?? ended
        break
      }
      if (step.value.stream !== 'stdout') continue
      pending += step.value.text
      const lines = pending.split('\n')
      pending = lines.pop() ?? ''
      for (const line of lines) {
        if (line === 'connected') retryMs = FIRST_RETRY_MS
        else if (line.trim() !== '') await receive($, line)
      }
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
    const key = await launchKey($)
    session = { id, alondra: $.plugin.root + '/bin/alondra', version, cwd: e.cwd, launchKey: key }
    queue($, { event: 'session.start', version, cwd: e.cwd, launch_key: key })
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
    if (!('agentId' in e && e.agentId !== undefined)) turnId = e.turnId
    queue($, { event: 'turn.start', turn_id: e.turnId })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (e.turnId === turnId) turnId = undefined
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

  on('tool.call', { tool: 'AskUserQuestion' }, async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const reply = await hold($, next, { id: e.tool_use_id, kind: 'question', input: { questions: e.questions } })
    if (!reply.held) return next(e)
    return { result: { questions: e.questions, answers: reply.answers } }
  }).catch(async (_$, _e, next) => ({ deny: `The question could not be put to the user: ${next.error.kind}` }))

  on('tool.call', { tool: 'ExitPlanMode' }, async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const reply = await hold($, next, { id: e.tool_use_id, kind: 'plan', input: { plan: e.plan } })
    if (!reply.held) return next(e)
    if (!reply.approved) return { deny: reply.feedback ?? 'The user requests changes to the plan' }
    approvedPlans.add(e.tool_use_id)
    return next(e)
  }).catch(async (_$, _e, next) => ({ deny: `The plan could not be put to the user: ${next.error.kind}` }))

  on('tool.check', async ($, e, next) => {
    const decided = await next(e)
    if (e.tool === 'ExitPlanMode' && approvedPlans.delete(e.tool_use_id)) {
      return { decision: 'allow', reason: 'The user approved this plan' }
    }
    if (decided.decision !== 'ask' || e.tool === 'AskUserQuestion') return decided
    const reply = await hold($, next, { id: e.tool_use_id, kind: 'permission', tool: e.tool, input: e.input })
    if (!reply.held) return decided
    return reply.allowed
      ? { decision: 'allow', reason: reply.reason ?? 'The user allowed this call' }
      : { decision: 'deny', reason: reply.reason ?? 'The user denies it' }
  }).catch(async (_$, _e, next) => ({
    decision: 'deny',
    reason: `The permission question could not be put to the user: ${next.error.kind}`,
  }))
}
