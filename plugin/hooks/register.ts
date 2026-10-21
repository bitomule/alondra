import type { EngineInterface, ProcessSpawnResult, Register } from 'claude-code'

const REPORT_TIMEOUT_MS = 10_000
const FIRST_RETRY_MS = 1_000
const MAX_RETRY_MS = 30_000
const ANSWER_LIMIT = 8_000
const SUMMARY_FIELDS = ['command', 'file_path', 'description', 'pattern', 'url'] as const
const SUMMARY_LIMIT = 300

const HOLD_RUN_MS = 600_000

type Session = { id: string; alondra: string; version: string; cwd: string; launchKey?: string }

type Card = { id: string; kind: 'question' | 'plan'; input: Record<string, unknown>; cwd?: string }
type Answer = {
  held: boolean
  answers?: Record<string, string>
  approved?: boolean
  feedback?: string
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
type Message = { id: string; text: string; asUser: boolean }

// Claude Code runs a plugin's prompt only once the session is idle, so a message that arrives
// mid-turn waits here and rides on the next tool result of the main loop.
const waiting = new Map<string, Message>()

const CARD_TOOLS = new Set([
  'mcp__plugin_alondra_alondra__ask',
  'mcp__plugin_alondra_alondra__request_test',
  'mcp__plugin_alondra_alondra__request_action',
  'mcp__plugin_alondra_alondra__request_input',
  'mcp__plugin_alondra_alondra__present_result',
  'AskUserQuestion',
  'ExitPlanMode',
])

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
    if (turnId !== undefined && !submitted.has(push.id)) {
      waiting.set(push.id, { id: push.id, text: push.text, asUser: push.as_user === true })
    } else {
      void submit($, push.id, push.text, push.as_user === true)
    }
  }
}

function takeWaiting(): Message[] {
  const messages = [...waiting.values()]
  waiting.clear()
  return messages
}

function said(messages: Message[]): string {
  return messages.map((message) => message.text).join('\n\n')
}

// The model has the messages in front of it: they show in the session as a notice and count as
// read for alondrad.
async function delivered($: EngineInterface, messages: Message[]): Promise<void> {
  for (const message of messages) submitted.add(message.id)
  try {
    await $.session.append({ message: { type: 'system', content: [{ type: 'text', text: said(messages) }] } })
  } catch (error) {
    $.ui.log(`Alondra messages were not shown in the session: ${error}`, { to: 'debug' })
  }
  for (const message of messages) await acknowledge($, message.id)
}

function flushWaiting($: EngineInterface): void {
  for (const message of takeWaiting()) void submit($, message.id, message.text, message.asUser)
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

let reportedModel: string | undefined
const subagentModels = new Map<string, string | undefined>()

type Fields = Record<string, unknown>

function text(value: unknown): string | undefined {
  return typeof value === 'string' && value !== '' ? value.slice(0, SUMMARY_LIMIT) : undefined
}

function step(id: unknown, fields: Fields): Fields | undefined {
  if (typeof id !== 'string' && typeof id !== 'number') return undefined
  return {
    id: String(id),
    subject: text(fields.subject ?? fields.content),
    status: text(fields.status),
    active_form: text(fields.activeForm),
  }
}

function steps(list: unknown, id: (entry: Fields, index: number) => unknown): Fields[] {
  if (!Array.isArray(list)) return []
  return list.flatMap((entry: Fields, index) => step(id(entry, index), entry) ?? [])
}

function progress(call: Fields, result: Fields): Fields | undefined {
  switch (call.tool) {
    case 'TaskCreate': {
      const created = step((result.task as Fields | undefined)?.id, call)
      return created && { event: 'step.create', ...created }
    }
    case 'TaskUpdate': {
      const updated = result.success === false ? undefined : step(call.taskId, call)
      return updated && { event: 'step.update', ...updated }
    }
    case 'TaskList':
      return Array.isArray(result.tasks) ? { event: 'steps', steps: steps(result.tasks, (entry) => entry.id) } : undefined
    case 'TodoWrite':
      return call.agentId === undefined ? { event: 'steps', steps: steps(call.todos, (_, index) => index + 1) } : undefined
    case 'Workflow': {
      const id = text(result.taskId)
      if (!id || call.agentId !== undefined) return undefined
      const name = text(result.workflowName) ?? text(call.name) ?? 'workflow'
      return { event: 'workflow.start', id, name, transcript_dir: text(result.transcriptDir) }
    }
  }
  return undefined
}

const PROGRESS_TOOLS = ['TaskCreate', 'TaskUpdate', 'TaskList', 'TodoWrite', 'Workflow'] as const

export const register: Register = (on) => {
  on('turn.step', async function* ($, e, next) {
    if (e.agentId === undefined && e.model !== reportedModel) {
      reportedModel = e.model
      queue($, { event: 'model', model: e.model })
    } else if (e.agentId !== undefined && subagentModels.has(e.agentId) && subagentModels.get(e.agentId) !== e.model) {
      subagentModels.set(e.agentId, e.model)
      queue($, { event: 'model', model: e.model, agent_id: e.agentId })
    }
    return yield* next(e)
  })

  on('session.start', async ($, e, next) => {
    reportedModel = undefined
    subagentModels.clear()
    const started = await next(e)
    // Under the Agent SDK the host reports the session and puts its questions, plans and
    // permissions to the user; holding them here would wait on a daemon that never answers.
    if ((await $.env.get('ALONDRA_SDK_HOST')) === '1') {
      session = undefined
      return started
    }
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

  on('turn.start', async ($, e, next) => {
    if (!('agentId' in e && e.agentId !== undefined)) turnId = e.turnId
    queue($, { event: 'turn.start', turn_id: e.turnId })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (e.turnId === turnId) {
      turnId = undefined
      flushWaiting($)
    }
    if (e.agentId === undefined) {
      queue($, {
        event: 'turn.complete',
        turn_id: e.turnId,
        answer: e.answer.slice(0, ANSWER_LIMIT),
        aborted: e.isAborted,
      })
    } else {
      subagentModels.delete(e.agentId)
      queue($, { event: 'subagent.end', id: e.agentId })
    }
    return next(e)
  })

  on('agent.spawn', async ($, e, next) => {
    const started = await next(e)
    if (started.agentId !== undefined) {
      subagentModels.set(started.agentId, started.model)
      queue($, {
        event: 'subagent.start',
        id: started.agentId,
        description: e.description,
        type: e.subagentType,
        model: started.model,
      })
    }
    return started
  })

  on('classic.Stop', async ($, e, next) => {
    queue($, {
      event: 'background',
      tasks: (e.background_tasks ?? []).map((task) => ({
        id: task.id,
        type: task.type,
        description: task.description,
        command: task.command,
        agent_type: task.agent_type,
        name: task.name,
      })),
    })
    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    queue($, { event: 'tool.start', tool: e.tool, tool_use_id: e.tool_use_id, input: summaryInput(e) })
    try {
      if (CARD_TOOLS.has(e.tool) && waiting.size > 0) {
        const unread = takeWaiting()
        void delivered($, unread)
        return {
          deny: 'Not sent: messages reached you while you worked and you had not read them yet. ' +
            `Read them and decide whether this is still needed:\n\n${said(unread)}`,
        }
      }
      const result = await next(e)
      if (e.tool === 'Bash' && e.run_in_background === true && !result.deny && !result.isError) {
        queue($, { event: 'shell.start', id: e.tool_use_id, command: String(e.command) })
      }
      if (result.deny !== undefined || waiting.size === 0) return result
      const arrived = takeWaiting()
      void delivered($, arrived)
      return {
        ...result,
        context: [...(result.context ?? []), `Messages reached you while this tool ran; act on them before you go on:\n\n${said(arrived)}`],
      }
    } finally {
      queue($, { event: 'tool.end', tool_use_id: e.tool_use_id })
    }
  })

  for (const tool of PROGRESS_TOOLS) {
    on('tool.call', { tool }, async ($, e, next) => {
      const done = await next(e)
      if (done.deny === undefined && !done.isError && typeof done.result === 'object' && done.result !== null) {
        const report = progress(e as Fields, done.result as Fields)
        if (report) queue($, report)
      }
      return done
    })
  }

  on('tool.call', { tool: 'AskUserQuestion' }, async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const reply = await hold($, next, { id: e.tool_use_id, kind: 'question', input: { questions: e.questions } })
    if (!reply.held) return next(e)
    return { result: { questions: e.questions, answers: reply.answers } }
  }).catch(async (_$, _e, next) => ({ deny: `The question could not be put to the user: ${next.error.kind}` }))

  on('tool.call', { tool: 'ExitPlanMode' }, async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const reply = await hold($, next, { id: e.tool_use_id, kind: 'plan', cwd: session?.cwd, input: { plan: e.plan } })
    if (!reply.held) return next(e)
    if (!reply.approved) return { deny: reply.feedback ?? 'The user requests changes to the plan' }
    approvedPlans.add(e.tool_use_id)
    return next(e)
  }).catch(async (_$, _e, next) => ({ deny: `The plan could not be put to the user: ${next.error.kind}` }))

  on('tool.check', async ($, e, next) => {
    if (e.tool === 'ExitPlanMode' && approvedPlans.delete(e.tool_use_id)) {
      return { decision: 'allow', reason: 'The user approved this plan' }
    }
    return next(e)
  })
}
