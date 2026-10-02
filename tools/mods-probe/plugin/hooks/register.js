let mainTurns = 0

async function record($, ev, data) {
  const path = (await $.env.get('MODS_PROBE_LOG')) || '/tmp/alondra-mods-probe.log'
  const line = JSON.stringify({ t: new Date().toISOString(), src: 'mod', ev, ...data })
  try {
    await $.process.run(['/bin/sh', '-c', 'printf "%s\\n" "$1" >> "$2"', 'probe', line, path])
  } catch (err) {
    $.ui.log('probe log failed: ' + err, { to: 'debug' })
  }
}

async function submitProbe($) {
  await record($, 'prompt.submit.call', {})
  try {
    await $.prompt.submit({ text: 'MODS-PROBE-SUBMIT: reply with the single word PONG and nothing else.', asUser: true })
    await record($, 'prompt.submit.resolved', {})
  } catch (err) {
    await record($, 'prompt.submit.rejected', { error: String(err) })
  }
}

async function compactProbe($) {
  await record($, 'session.compact.call', {})
  try {
    const r = await $.session.compact({ instructions: 'Keep the line MODS-PROBE-KEEP.' })
    await record($, 'session.compact.resolved', { skip: r.skip, tokensBefore: r.tokensBefore, tokensAfter: r.tokensAfter })
  } catch (err) {
    await record($, 'session.compact.rejected', { error: String(err) })
  }
}

export function register(on) {
  on('session.start', async ($, e, next) => {
    const id = await $.session.id()
    const version = await $.session.version()
    const usage = await $.session.usage()
    await record($, 'session.start', { id, version: version.version, cwd: e.cwd, surface: e.surface, isInteractive: e.isInteractive, context: usage.context })
    $.clock.after(15_000, async () => {
      await record($, 'clock.after', { surfaces: await $.session.surfaces() })
    })
    return next(e)
  })

  on('session.attach', async ($, e, next) => {
    await record($, 'session.attach', { e })
    return next(e)
  })

  on('session.detach', async ($, e, next) => {
    await record($, 'session.detach', { e })
    return next(e)
  })

  on('prompt.submit', async ($, e, next) => {
    await record($, 'prompt.submit', { text: String(e.text).slice(0, 80), origin: e.origin })
    return next(e)
  })

  on('turn.start', async ($, e, next) => {
    await record($, 'turn.start', { turnId: e.turnId, agentId: e.agentId })
    return next(e)
  })

  on('turn.complete', async ($, e, next) => {
    await record($, 'turn.complete', { agentId: e.agentId, isAborted: e.isAborted, answer: String(e.answer ?? '').slice(0, 200) })
    if (!e.agentId) {
      mainTurns += 1
      if (mainTurns === 1) $.clock.after(5_000, () => submitProbe($))
      if (mainTurns === 2) $.clock.after(5_000, () => compactProbe($))
    }
    return next(e)
  })

  on('session.measure', async ($, e, next) => {
    await record($, 'session.measure', { changed: e.changed, context: e.context, rateLimits: e.rateLimits })
    return next(e)
  })

  on('session.compact', async ($, e, next) => {
    await record($, 'session.compact', { trigger: e.trigger, agentId: e.agentId, instructions: e.instructions })
    return next(e)
  })

  on('agent.spawn', async ($, e, next) => {
    const patch = e.prompt.includes('MODPATCH')
    await record($, 'agent.spawn', { subagentType: e.subagentType, parentAgentId: e.parentAgentId, background: e.background, permissionMode: e.permissionMode, patched: patch })
    const result = await next(patch ? { ...e, prompt: 'MODCLAIM-7\n' + e.prompt } : e)
    await record($, 'agent.spawn.result', { agentId: result.agentId, deny: result.deny })
    return result
  })

  on('tool.call', async ($, e, next) => {
    const result = await next(e)
    await record($, 'tool.call', { tool: e.tool, agentId: e.agentId, deny: result.deny, isError: result.isError })
    return result
  })
}
