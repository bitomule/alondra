import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { probeModels } from './model-probe.js';

const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'model-catalog-'));
const capture = join(root, 'requests.jsonl');
const fixture = fileURLToPath(new URL('./fake-model-catalog.mjs', import.meta.url));
const closed = [];
try {
  const report = await probeModels({ executable: '/fixture/claude', cwd: root, env: { HOME: root, FIXTURE_CAPTURE: capture }, timeout_ms: 10_000 }, (options) => {
    const child = spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'pipe'] });
    closed.push(new Promise((resolve) => child.once('close', resolve)));
    return child;
  });
  await Promise.all(closed);
  assert.equal(report.native_default, 'default');
  assert.deepEqual(report.models.map((model) => [model.model, model.is_default]), [['native-first', false], ['default', true], ['native-second', false]]);
  assert.deepEqual(report.models[0].reasoning_efforts, ['low', 'high']);
  assert.equal(report.models[0].context_window, null);
  const requests = readFileSync(capture, 'utf8').trim().split('\n').map(JSON.parse);
  assert.equal(requests.length, 1);
  assert.equal(requests[0].request.subtype, 'initialize');
  assert.equal(requests.some((request) => request.type === 'user'), false);
} finally { await Promise.all(closed); rmSync(root, { recursive: true, force: true }); }
