import assert from 'node:assert/strict';
import { existsSync, readdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import { request } from 'node:http';
import sdkPackage from '../../../agent-host/node_modules/@anthropic-ai/claude-agent-sdk/package.json';

const [configPath] = process.argv.slice(2);
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const expectedEnvironment = Object.freeze({ ...config.env });
const children = [];
const sessions = [];
const cleanupErrors = [];
let primary;
let report = { status: 'failed', actualSpawns: [], exits: [], steps: [] };
const abort = new AbortController();
const signalHandler = () => abort.abort(new Error('Harness interrupted'));
process.on('SIGTERM', signalHandler);
process.on('SIGINT', signalHandler);

function bounded(promise, label) {
  let timer;
  return Promise.race([promise, new Promise((_, reject) => {
    timer = setTimeout(() => reject(new Error(`Hang diagnostic: ${label}`)), 120000);
  })]).finally(() => clearTimeout(timer));
}
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
function step(name, value) { report.steps.push({ name, ...value }); }

function actualSpawn(options) {
  assert.ok(children.length < 3, 'At most one native Claude per session');
  assert.equal(options.command, config.native);
  assert.equal(options.cwd, config.work);
  assert.deepEqual(options.env, { ...expectedEnvironment, CLAUDE_CODE_ENTRYPOINT: 'sdk-ts', CLAUDE_AGENT_SDK_VERSION: '0.3.300', CLAUDE_CODE_SDK_READS_SESSION_STATE: '1' }, 'No inherited credentials, proxy, OAuth or helper environment');
  const child = spawn(options.command, options.args, { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'pipe'] });
  child.stderr.on('data', (data) => process.stderr.write(data));
  const record = { child, started: false, exited: false };
  record.exit = new Promise((resolve) => {
    child.once('spawn', () => { record.started = true; report.actualSpawns.push(child.pid); });
    child.once('exit', (code, signal) => { record.exited = true; const value = { pid: child.pid, actualExit: true, code, signal }; report.exits.push(value); resolve(value); });
    child.once('error', (error) => {
      cleanupErrors.push(`Native process error: ${error.message}`);
      if (child.pid === undefined) resolve({ notSpawned: true });
    });
  });
  children.push(record);
  return child;
}

/** A session that sends Claude no prompt: MCP and its controls need none, and there is no model. */
function session(query) {
  let release;
  const held = new Promise((resolve) => { release = resolve; });
  async function* input() { await held; }
  const sdk = query({ prompt: input(), options: {
    cwd: config.work, env: { ...expectedEnvironment }, pathToClaudeCodeExecutable: config.native,
    persistSession: false, permissionMode: 'default', settingSources: ['user', 'project', 'local'],
    tools: { type: 'preset', preset: 'claude_code' }, systemPrompt: { type: 'preset', preset: 'claude_code' },
    abortController: abort, spawnClaudeCodeProcess: actualSpawn, stderr: (data) => process.stderr.write(data),
    extraArgs: { 'debug-to-stderr': null },
  } });
  const drained = (async () => { for await (const _ of sdk) { /* no prompt, no messages expected */ } })().catch((error) => {
    if (!abort.signal.aborted) cleanupErrors.push(`Session stream: ${error.message}`);
  });
  const value = { sdk, release, drained };
  sessions.push(value);
  return value;
}

async function fixtureStatus(sdk) {
  const servers = await bounded(sdk.mcpServerStatus(), 'mcp status');
  const fixture = servers.find((server) => server.name === 'fixture');
  assert.ok(fixture, `Claude lists the configured server: ${JSON.stringify(servers.map((s) => s.name))}`);
  return fixture;
}
/** Asks again, a bounded number of times, until the server is no longer `pending`: a hang is a failure. */
async function settled(sdk, until = (status) => status !== 'pending') {
  let last;
  for (let attempt = 0; attempt < 120; attempt += 1) {
    last = await fixtureStatus(sdk);
    if (until(last.status)) return last;
    await pause(250);
  }
  throw new Error(`Hang diagnostic: the MCP server never settled (${last?.status}: ${last?.error ?? ''})`);
}

function get(url, headers = {}) {
  return new Promise((resolve, reject) => {
    const outgoing = request(url, { method: 'GET', headers }, (response) => {
      let body = '';
      response.on('data', (chunk) => { body += chunk; if (body.length > 65536) response.destroy(new Error('Oversized response')); });
      response.once('error', reject);
      response.once('end', () => resolve({ status: response.statusCode, location: response.headers.location, body }));
    });
    outgoing.once('error', reject);
    outgoing.end();
  });
}

function storedSignIn() {
  if (!existsSync(config.keychain)) return { items: 0 };
  const items = readdirSync(config.keychain).map((name) => JSON.parse(readFileSync(join(config.keychain, name), 'utf8')));
  const signIns = items.flatMap((item) => Object.entries(item.mcpOAuth ?? {}));
  return {
    items: items.length,
    sections: [...new Set(items.flatMap((item) => Object.keys(item)))].sort(),
    keys: signIns.map(([key]) => key.replace(/\|[0-9a-f]+$/, '|<hash>')),
    fields: signIns.length ? Object.keys(signIns[0][1]).sort() : [],
  };
}

async function main() {
  try {
    assert.equal(process.version, 'v24.19.0');
    assert.equal(sdkPackage.version, '0.3.300');
    assert.deepEqual({ ...process.env }, expectedEnvironment, 'Sandboxed client also receives only the fixture environment');
    const { query } = await import('../../../agent-host/node_modules/@anthropic-ai/claude-agent-sdk/core.mjs');

    const first = session(query);
    const second = session(query);
    const before = await settled(first.sdk);
    const beforeSecond = await settled(second.sdk);
    step('status before signing in', { first: before.status, second: beforeSecond.status, transport: before.config?.type, source: before.source, scope: before.scope });
    assert.equal(before.status, 'needs-auth');

    const signIn = await bounded(first.sdk.mcpAuthenticate('fixture'), 'mcp_authenticate');
    step('mcp_authenticate', { keys: Object.keys(signIn).sort(), redirectScheme: signIn.redirectScheme, callbackExpected: signIn.callbackExpected, requiresUserAction: signIn.requiresUserAction, callbackPort: signIn.callbackPort, hasState: typeof signIn.state === 'string' });
    assert.equal(signIn.callbackPort, Number(config.env.MCP_OAUTH_CALLBACK_PORT), 'Claude listens where MCP_OAUTH_CALLBACK_PORT says');

    const page = await bounded(get(signIn.authUrl), 'authorization page');
    assert.equal(page.status, 302, 'The authorization server sends the browser back');
    const back = new URL(page.location);
    step('browser sent back', { host: back.host, path: back.pathname, params: [...back.searchParams.keys()].sort(), stateMatches: back.searchParams.get('state') === signIn.state });
    assert.equal(back.pathname, '/callback');

    const delivered = await bounded(get(`http://127.0.0.1:${back.port}${back.pathname}${back.search}`, { host: `localhost:${back.port}` }), 'Claude listener');
    step('return handed to Claude listener', { status: delivered.status });
    assert.ok(delivered.status >= 200 && delivered.status < 400, `Claude took the return: ${delivered.status}`);

    let after = await settled(first.sdk, (status) => status === 'connected' || status === 'failed').catch((error) => ({ status: 'unsettled', error: error.message }));
    step('status after the listener took the return', { status: after.status, tools: (after.tools ?? []).map((tool) => tool.name) });
    if (after.status !== 'connected') {
      const submit = first.sdk.mcpSubmitOAuthCallbackUrl;
      const answered = await bounded(submit.call(first.sdk, 'fixture', page.location).then((value) => ({ value }), (error) => ({ error: error.message })), 'mcp_oauth_callback_url');
      step('mcp_oauth_callback_url', { answered: answered.error ? undefined : Object.keys(answered.value ?? {}), error: answered.error });
      after = await settled(first.sdk, (status) => status === 'connected' || status === 'failed');
      step('status after mcp_oauth_callback_url', { status: after.status, tools: (after.tools ?? []).map((tool) => tool.name) });
    }
    assert.equal(after.status, 'connected');
    step('credential store', storedSignIn());

    const secondBeforeReconnect = await fixtureStatus(second.sdk);
    await bounded(second.sdk.reconnectMcpServer('fixture').catch((error) => step('second reconnect error', { error: error.message })), 'second reconnect');
    const secondAfter = await fixtureStatus(second.sdk);
    step('another session reconnects at once', { before: secondBeforeReconnect.status, after: secondAfter.status });

    const third = session(query);
    const fresh = await settled(third.sdk);
    step('a new session starts', { status: fresh.status });
    assert.equal(fresh.status, 'connected', 'A new session uses the stored sign-in');

    await bounded(get(`${config.server}/fixture/revoke`), 'revoke');
    await bounded(third.sdk.reconnectMcpServer('fixture').catch((error) => step('reconnect after revocation error', { error: error.message })), 'reconnect after revocation');
    const revoked = await settled(third.sdk, (status) => status !== 'pending');
    step('after the server stops accepting the sign-in', { status: revoked.status, error: revoked.error });
    report = { ...report, status: 'passed', sdkVersion: sdkPackage.version };
  } catch (error) {
    primary = error;
  } finally {
    for (const value of sessions) {
      try { value.release(); value.sdk.close(); } catch (error) { cleanupErrors.push(`Query close: ${error.message}`); }
    }
    for (const value of sessions) await value.drained;
    for (const record of children) {
      try {
        const exit = await bounded(record.exit, 'actual native exit barrier');
        if (record.started) assert.equal(exit.actualExit, true);
      } catch (error) {
        cleanupErrors.push(`Native exit: ${error.message}`);
        if (record.started && !record.exited) {
          try { record.child.kill('SIGKILL'); await record.exit; }
          catch (stopError) { cleanupErrors.push(`Final fixture stop: ${stopError.message}`); }
        }
      }
    }
    report.cleanupKnown = children.every((record) => record.exited || record.child.pid === undefined);
    report.cleanupErrors = cleanupErrors;
    if (primary || cleanupErrors.length) report.status = 'failed';
    report.error = primary?.stack;
    try { writeFileSync(config.report, JSON.stringify(report)); }
    catch (error) { cleanupErrors.push(`Report write: ${error.message}`); }
    process.off('SIGTERM', signalHandler);
    process.off('SIGINT', signalHandler);
  }
  if (primary) throw primary;
  if (cleanupErrors.length) throw new Error(cleanupErrors.join('; '));
}
main().catch((error) => { console.error(error); process.exitCode = 1; });
