import assert from 'node:assert/strict';
import { readFileSync, writeFileSync } from 'node:fs';
import { spawn } from 'node:child_process';
import { connect } from 'node:net';
import { get } from 'node:http';
import { query } from '../../../agent-host/node_modules/@anthropic-ai/claude-agent-sdk/core.mjs';
import sdkPackage from '../../../agent-host/node_modules/@anthropic-ai/claude-agent-sdk/package.json';

const [configPath, mode] = process.argv.slice(2);
const config = JSON.parse(readFileSync(configPath, 'utf8'));
const expectedEnvironment = Object.freeze({ ...config.env });
const children = [];
const cleanupErrors = [];
let sdkQuery;
let primary;
let report = { mode, status: 'failed', actualSpawns: [], exits: [] };
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
function deniedConnection(port) {
  return new Promise((resolve, reject) => {
    const socket = connect({ host: '127.0.0.1', port });
    socket.once('connect', () => { socket.destroy(); reject(new Error('Sandbox allowed the unrelated owned listener')); });
    socket.once('error', (error) => {
      socket.destroy();
      try { assert.ok(['EPERM', 'EACCES'].includes(error.code), `Expected OS denial, got ${error.code}`); resolve(error.code); }
      catch (failure) { reject(failure); }
    });
  });
}
function allowedRequest() {
  return new Promise((resolve, reject) => {
    const request = get(config.baseUrl + '/isolation-ready/' + config.marker, (response) => {
      let data = '';
      response.on('data', (chunk) => { data += chunk; if (data.length > 1024) response.destroy(new Error('Oversized readiness response')); });
      response.once('error', reject);
      response.once('end', () => {
        try { assert.equal(response.statusCode, 200); assert.equal(JSON.parse(data).marker, config.marker); resolve(); }
        catch (error) { reject(error); }
      });
    });
    request.once('error', reject);
  });
}
function actualSpawn(options) {
  assert.equal(children.length, 0, 'The SDK may create exactly one native child');
  assert.equal(options.command, config.native);
  assert.equal(options.cwd, config.work);
  assert.ok(!options.args.some((arg) => ['--bare', '--simple'].includes(arg)));
  assert.deepEqual(options.env, { ...expectedEnvironment, CLAUDE_CODE_ENTRYPOINT: 'sdk-ts' }, 'No inherited credentials, proxy, OAuth or helper environment');
  const child = spawn(options.command, options.args, { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'pipe'] });
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

async function main() {
  try {
    assert.equal(process.version, 'v24.19.0');
    assert.equal(sdkPackage.version, '0.3.300');
    assert.deepEqual({ ...process.env }, expectedEnvironment, 'Sandboxed client also receives only the fixture environment');
    if (mode === 'isolation') {
      assert.equal(readFileSync(config.allowedFile, 'utf8'), config.marker);
      let readCode;
      try { readFileSync(config.deniedFile); assert.fail('Sandbox allowed the owned sibling canary'); }
      catch (error) { assert.ok(['EPERM', 'EACCES'].includes(error.code)); readCode = error.code; }
      const connectCode = await bounded(deniedConnection(config.deniedPort), 'unrelated listener denial');
      await bounded(allowedRequest(), 'owned model listener readiness');
      report = { ...report, readCode, connectCode, status: 'passed' };
    } else {
      assert.equal(mode, 'query');
      const messages = [];
      sdkQuery = query({ prompt: config.prompt, options: {
        cwd: config.work, env: { ...expectedEnvironment }, pathToClaudeCodeExecutable: config.native,
        model: 'claude-sonnet-4-6', maxTurns: 1, persistSession: false,
        permissionMode: 'default', settingSources: ['user', 'project', 'local'],
        tools: { type: 'preset', preset: 'claude_code' },
        systemPrompt: { type: 'preset', preset: 'claude_code' },
        abortController: abort, spawnClaudeCodeProcess: actualSpawn,
        stderr: (data) => process.stderr.write(data),
      } });
      await bounded((async () => {
        for await (const message of sdkQuery) {
          assert.ok(messages.length < 256, 'Bounded actual SDK message history');
          messages.push(message);
          if (message.type === 'result') break;
        }
      })(), 'actual SDK assistant/result');
      const init = messages.find((message) => message.type === 'system' && message.subtype === 'init');
      assert.equal(init?.claude_code_version, '2.1.300');
      const assistant = messages.filter((message) => message.type === 'assistant').flatMap((message) => message.message.content).filter((block) => block.type === 'text').map((block) => block.text).join('');
      assert.equal(assistant, config.response);
      const results = messages.filter((message) => message.type === 'result');
      assert.equal(results.length, 1);
      assert.equal(results[0].subtype, 'success');
      assert.equal(results[0].is_error, false);
      assert.equal(results[0].result, config.response);
      assert.equal(report.actualSpawns.length, 1);
      report = { ...report, status: 'passed', sdkVersion: sdkPackage.version, nativeVersion: init.claude_code_version, assistant, result: results[0].result };
    }
  } catch (error) {
    primary = error;
  } finally {
    try { sdkQuery?.close(); } catch (error) { cleanupErrors.push(`Query close: ${error.message}`); }
    for (const record of children) {
      if (cleanupErrors.length && record.child.pid !== undefined && !record.exited) {
        try { record.child.kill('SIGTERM'); } catch (error) { cleanupErrors.push(`Fixture stop: ${error.message}`); }
      }
      try {
        const exit = await bounded(record.exit, 'actual native exit barrier');
        if (record.started) { assert.equal(exit.actualExit, true); assert.equal(exit.code, 0); }
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
