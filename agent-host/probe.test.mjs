import assert from 'node:assert/strict';
import { existsSync, mkdtempSync, readFileSync, realpathSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { probe } from './probe.js';

const root = mkdtempSync(join(realpathSync('/tmp'), 'probe-'));
const executable = fileURLToPath(new URL('./persistent-fake-claude.py', import.meta.url));
const lines = (path) => existsSync(path) ? readFileSync(path, 'utf8').split('\n').filter(Boolean).map((line) => JSON.parse(line)) : [];

try {
  const capture = join(root, 'capture.json');
  const env = { HOME: root, PATH: '/usr/bin:/bin', FIXTURE_CAPTURE: capture, FIXTURE_MCP_CONNECTING: '2' };
  const servers = await probe({ executable, cwd: root, env });
  assert.deepEqual(servers, [
    { name: 'fixture', status: 'connected' },
    { name: 'linear', status: 'needs-auth', transport: 'http', url: 'https://mcp.linear.app/mcp' },
  ], 'the servers once those still connecting settled, the address without its query');
  assert.deepEqual(lines(capture + '.controls'), [{ subtype: 'mcp_status' }, { subtype: 'mcp_status' }, { subtype: 'mcp_status' }], 'it only asks how the servers are');
  assert.equal(existsSync(capture + '.prompts'), false, 'no message ever reaches Claude');
  assert.deepEqual(lines(capture + '.starts').map((start) => start.cwd), [root], 'Claude starts in the project');
  assert.equal(JSON.parse(readFileSync(capture, 'utf8')).args.includes('--no-session-persistence'), true, 'and keeps no session of it');
} finally {
  rmSync(root, { recursive: true, force: true });
}
