const SOCKET = '/tmp/alondra-push-probe.sock'
const LOG = '/tmp/alondra-push-probe.log'
const MAX_BACKOFF_MS = 30_000

let backoffMs = 1_000
let deliveries = Promise.resolve()

async function record($, ev, data) {
  const line = JSON.stringify({ t: new Date().toISOString(), ev, ...data })
  try {
    await $.process.run(['/bin/sh', '-c', 'printf "%s\\n" "$1" >> "$2"', 'probe', line, LOG], { timeoutMs: 5_000 })
  } catch (err) {
    $.ui.log('push probe log failed: ' + err, { to: 'debug' })
  }
}

async function deliver($, session, message) {
  await record($, 'received', { id: message.id, text: message.text.slice(0, 80) })
  await $.prompt.submit({ text: message.text, asUser: true })
  await record($, 'submitted', { id: message.id })
  try {
    await $.process.run(['python3', $.plugin.root + '/bin/push-ack.py', SOCKET, session, String(message.id)], { timeoutMs: 10_000 })
    await record($, 'acked', { id: message.id })
  } catch (err) {
    await record($, 'ack.failed', { id: message.id, error: String(err) })
  }
}

async function listen($, session) {
  await record($, 'spawn', { backoffMs })
  let pending = ''
  let ended = { code: null, signal: null }
  try {
    const helper = $.process.spawn({ argv: ['python3', $.plugin.root + '/bin/push-helper.py', SOCKET, session] })
    for (;;) {
      const step = await helper.next()
      if (step.done) {
        ended = step.value ?? ended
        break
      }
      if (step.value.stream === 'stderr') {
        await record($, 'helper.stderr', { text: step.value.text.slice(0, 200) })
        continue
      }
      pending += step.value.text
      const lines = pending.split('\n')
      pending = lines.pop()
      for (const line of lines) {
        if (line === 'connected') {
          backoffMs = 1_000
          await record($, 'connected', {})
          continue
        }
        const message = JSON.parse(line)
        deliveries = deliveries.then(() => deliver($, session, message))
      }
    }
  } catch (err) {
    await record($, 'spawn.failed', { error: String(err) })
  }
  await record($, 'helper.ended', { code: ended.code, signal: ended.signal, retryInMs: backoffMs })
  const wait = backoffMs
  backoffMs = Math.min(backoffMs * 2, MAX_BACKOFF_MS)
  $.clock.after(wait, () => listen($, session))
}

export function register(on) {
  on('session.start', async ($, e, next) => {
    const session = await $.session.id()
    await record($, 'session.start', { session, cwd: e.cwd })
    void listen($, session)
    return next(e)
  })

  on('turn.start', async ($, e, next) => {
    await record($, 'turn.start', { agentId: e.agentId })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    if (!e.agentId) await record($, 'turn.complete', { answer: String(e.answer ?? '').slice(0, 120) })
    return next(e)
  })
}
