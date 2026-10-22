import assert from 'node:assert/strict';
import { lstatSync, readFileSync, readlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawn } from 'node:child_process';
import sdkPackage from '../../../agent-host/node_modules/@anthropic-ai/claude-agent-sdk/package.json';

const [configPath] = process.argv.slice(2);
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const children = [];
const cleanupErrors = [];
let primary;
let expected;
let report = { status: 'failed', actualSpawns: [], exits: [], turns: [], links: {} };
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

function actualSpawn(options) {
  assert.ok(children.length < 4, 'One native Claude per turn');
  assert.equal(options.command, config.native);
  assert.equal(options.cwd, config.work);
  assert.deepEqual(options.env, { ...expected, CLAUDE_CODE_ENTRYPOINT: 'sdk-ts', CLAUDE_AGENT_SDK_VERSION: '0.3.300', CLAUDE_CODE_SDK_READS_SESSION_STATE: '1' }, 'Only the account\'s own environment');
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

/** What each shared name of an account folder is after a turn: still a link, and to where. */
function links(folder) {
  return Object.fromEntries(config.shared.map((name) => {
    const path = join(folder, name);
    try {
      const stat = lstatSync(path);
      return [name, stat.isSymbolicLink() ? { link: readlinkSync(path) } : { kind: stat.isDirectory() ? 'directory' : 'file' }];
    } catch (error) {
      return [name, { missing: error.code }];
    }
  }));
}

/** One turn of the account whose folder is `folder`, resuming `resume` when given. */
async function turn(query, account, prompt, resume) {
  expected = Object.freeze({ ...config.env, CLAUDE_CONFIG_DIR: config.accounts[account] });
  const messages = [];
  const sdk = query({ prompt, options: {
    cwd: config.work, env: { ...expected }, pathToClaudeCodeExecutable: config.native,
    maxTurns: 1, persistSession: true, ...(resume ? { resume } : {}),
    permissionMode: 'default', settingSources: ['user', 'project', 'local'],
    tools: { type: 'preset', preset: 'claude_code' }, systemPrompt: { type: 'preset', preset: 'claude_code' },
    abortController: abort, spawnClaudeCodeProcess: actualSpawn, stderr: (data) => process.stderr.write(data),
  } });
  try {
    await bounded((async () => {
      for await (const message of sdk) {
        assert.ok(messages.length < 256, 'Bounded actual SDK message history');
        messages.push(message);
        if (message.type === 'result') break;
      }
    })(), `turn of account ${account}`);
  } finally {
    sdk.close();
    const record = children[children.length - 1];
    if (record) await bounded(record.exit, 'native exit after the turn');
  }
  const init = messages.find((message) => message.type === 'system' && message.subtype === 'init');
  const result = messages.find((message) => message.type === 'result');
  const refusal = messages.find((message) => message.type === 'assistant' && message.error);
  const value = {
    account, resumed: resume ?? null, session: init?.session_id, resultSession: result?.session_id, model: init?.model,
    version: init?.claude_code_version, skills: init?.skills, agents: init?.agents, slashCommands: init?.slash_commands,
    result: result?.result, subtype: result?.subtype, isError: result?.is_error,
    error: refusal?.error ?? null, apiErrorStatus: result?.api_error_status ?? null, terminalReason: result?.terminal_reason ?? null,
    rateLimits: messages.filter((message) => message.type === 'rate_limit_event').map((message) => message.rate_limit_info),
  };
  report.turns.push(value);
  report.links[account + ' after its turn'] = links(config.accounts[account]);
  return value;
}

async function main() {
  try {
    assert.equal(process.version, 'v24.19.0');
    assert.equal(sdkPackage.version, '0.3.300');
    assert.deepEqual({ ...process.env }, config.env, 'The client receives only the fixture environment');
    const { query } = await import('../../../agent-host/node_modules/@anthropic-ai/claude-agent-sdk/core.mjs');
    report.links['a before any turn'] = links(config.accounts.a);
    const first = await turn(query, 'a', config.first.prompt);
    assert.equal(first.result, config.first.response);
    const second = await turn(query, 'b', config.second.prompt, first.session);
    assert.equal(second.result, config.second.response);
    const limited = await turn(query, 'a', config.limited.prompt);
    assert.equal(limited.isError, true, 'The usage limit refuses the turn');
    const after = await turn(query, 'b', config.after.prompt, limited.session);
    assert.equal(after.result, config.after.response);
    report.links['a after both turns'] = links(config.accounts.a);
    report = { ...report, status: 'passed', sdkVersion: sdkPackage.version };
  } catch (error) {
    primary = error;
  } finally {
    for (const record of children) {
      if (record.child.pid !== undefined && !record.exited) {
        try { record.child.kill('SIGTERM'); await bounded(record.exit, 'native exit at cleanup'); }
        catch (error) { cleanupErrors.push(`Native exit: ${error.message}`); }
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
