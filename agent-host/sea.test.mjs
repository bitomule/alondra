import assert from 'node:assert/strict';
import { copyFileSync, chmodSync, mkdtempSync, mkdirSync, readFileSync, writeFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';

const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'sea-'));
try {
  const original = fileURLToPath(new URL('./host', import.meta.url));
  const binary = join(root, 'host');
  copyFileSync(original, binary);
  chmodSync(binary, 0o700);
  copyFileSync(fileURLToPath(new URL('./fake-claude.py', import.meta.url)), join(root, 'fake.py'));
  const fake = join(root, 'claude');
  writeFileSync(fake, `#!/bin/sh\nexec /usr/bin/python3 -I '${join(root, 'fake.py').replaceAll("'", "'\\''")}' "$@"\n`, { mode: 0o700 });
  mkdirSync(join(root, 'node_modules'));
  const manifest = JSON.parse(readFileSync(fileURLToPath(new URL('./host.manifest.json', import.meta.url))));
  assert.equal(manifest.sha256, createHash('sha256').update(readFileSync(binary)).digest('hex'));
  const meta = JSON.parse(readFileSync(fileURLToPath(new URL('./host.meta.json', import.meta.url))));
  assert(meta.external.every((name) => !name.includes('anthropic') && !name.includes('modelcontextprotocol') && !name.includes('zod')));
  const result = spawnSync('/usr/bin/sandbox-exec', ['-p', '(version 1)(allow default)(deny network*)', binary, fake, root, 'success'], { cwd: root, env: { HOME: root, PATH: '/fixture/no-node-no-npm', NODE_PATH: join(root, 'trap-node-path'), NODE_OPTIONS: '--require=/fixture/trap.cjs' }, encoding: 'utf8' });
  assert.equal(result.error, undefined);
  assert.equal(result.status, 0, `${result.stderr}\n${result.stdout}`);
  const observations = result.stdout.trim().split('\n').map(JSON.parse);
  assert(observations.some((o) => o.type === 'sdk' && o.kind === 'initialized' && o.payload.session_id === 'sea-fixture'));
  assert(observations.some((o) => o.type === 'sdk' && o.kind === 'result' && o.payload.result === 'offline standalone SEA' && o.failures.length === 0));
  const signature = spawnSync('/usr/bin/codesign', ['--verify', '--strict', binary], { encoding: 'utf8' });
  assert.equal(signature.status, 0, signature.stderr);
  const missing = spawnSync(binary, [join(root, 'missing-claude'), root, 'missing'], { cwd: root, env: { HOME: root, PATH: '/fixture/empty' }, encoding: 'utf8' });
  assert.equal(missing.status, 1, missing.stderr);
  assert(missing.stdout.includes('iterator_error'));
  console.log(JSON.stringify({ standalone_sea: 'pass', offline: 'sandbox deny network', path: 'no Node/npm', sdk: manifest.sdk, node: manifest.node, sha256: manifest.sha256, signature: 'ad-hoc', bytes: readFileSync(binary).length }));
} finally { rmSync(root, { recursive: true, force: true }); }
