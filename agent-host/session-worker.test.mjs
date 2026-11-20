import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

// A task's native Claude loads every settings source of the user's default profile, the user's
// own MCP servers among them, and Alondra's node plugin and launch settings on top.
const fixture = fileURLToPath(new URL('./fake-claude.mjs', import.meta.url));
const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'sdk-worker-'));
const value = (args, flag) => {
  const inline = args.find((arg) => arg.startsWith(`${flag}=`));
  if (inline) return inline.slice(flag.length + 1);
  const at = args.indexOf(flag);
  return at === -1 ? undefined : args[at + 1];
};

// A task's Claude runs the model, effort and tools its launch chose, in the session the host was
// given: a new one, or the one it resumes.
const launch = async (name, { worker, resume = false, sessionId = '00000000-0000-4000-8000-0000000000b2' }) => {
  const { Session } = await import('./session.js');
  const home = join(root, `${name}-home`);
  mkdirSync(home);
  const capture = join(root, `${name}-capture.json`);
  let env;
  const run = new Session({
    executable: '/fixture/original-claude', cwd: root, sessionId, resume, worker,
    env: { HOME: home, PATH: '/fixture/no-js-runtime', FIXTURE_SCENARIO: 'success', FIXTURE_CAPTURE: capture, ALONDRA_DOMAIN_PROTOCOL: '2' },
    spawn: (options) => {
      env = options.env;
      return spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'inherit'] });
    },
    observe: (observation) => { if (observation.type === 'sdk' && observation.kind === 'result') run.close(); },
  });
  run.offer('00000000-0000-4000-8000-0000000000a1', 'the task');
  await run.finished;
  return { args: JSON.parse(readFileSync(capture)).args, env, home };
};

try {
  const { args, env, home } = await launch('profile', { worker: { plugins: ['/fixture/plugin'], settings: { env: { ALONDRA_PRODUCER: 'node' }, permissions: { deny: ['Bash(alondra answer:*)'] } } } });
  assert.equal(value(args, '--setting-sources'), 'user,project,local', args.join(' '));
  assert(!args.includes('--strict-mcp-config'), `the user's MCP servers load: ${args.join(' ')}`);
  assert.equal(value(args, '--mcp-config'), undefined, args.join(' '));
  assert.deepEqual(args.flatMap((arg, at) => arg.startsWith('--plugin-dir=') ? [arg.slice(13)] : args[at - 1] === '--plugin-dir' ? [arg] : []), ['/fixture/plugin'], args.join(' '));
  assert.deepEqual(JSON.parse(value(args, '--settings')).permissions.deny, ['Bash(alondra answer:*)']);
  assert.equal(value(args, '--permission-mode'), 'auto', args.join(' '));
  assert.equal(value(args, '--model'), undefined, `no model unless the launch chose one: ${args.join(' ')}`);
  assert.equal(value(args, '--effort'), undefined, `no effort unless the launch chose one: ${args.join(' ')}`);
  assert.equal(value(args, '--session-id'), '00000000-0000-4000-8000-0000000000b2', args.join(' '));
  assert.equal(value(args, '--resume'), undefined, args.join(' '));
  assert.equal(env.HOME, home);
  assert.equal(env.CLAUDE_CONFIG_DIR, undefined, 'the default profile');
  assert.equal(env.CLAUDE_HOME, undefined, 'the default profile');
  assert.equal(env.ALONDRA_DOMAIN_PROTOCOL, '2');

  const chosen = await launch('chosen', { worker: { plugins: [], settings: {}, model: 'opus', effort: 'xhigh', disallowed_tools: ['Bash', 'WebFetch'] } });
  assert.equal(value(chosen.args, '--model'), 'opus', chosen.args.join(' '));
  assert.equal(value(chosen.args, '--effort'), 'xhigh', chosen.args.join(' '));
  assert.deepEqual(value(chosen.args, '--disallowedTools')?.split(','), ['Bash', 'WebFetch'], chosen.args.join(' '));

  const resumed = await launch('resumed', { worker: { plugins: [], settings: {} }, resume: true, sessionId: '00000000-0000-4000-8000-0000000000B3' });
  assert.equal(value(resumed.args, '--resume'), '00000000-0000-4000-8000-0000000000b3', `it resumes its session, by its lowercase id: ${resumed.args.join(' ')}`);
  assert.equal(value(resumed.args, '--session-id'), undefined, resumed.args.join(' '));
} finally {
  rmSync(root, { recursive: true, force: true });
}
