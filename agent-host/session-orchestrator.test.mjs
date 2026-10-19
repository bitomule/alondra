import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, mkdirSync, readFileSync, rmSync } from 'node:fs';
import { join } from 'node:path';
import { fileURLToPath } from 'node:url';

const fixture = fileURLToPath(new URL('./fake-claude.mjs', import.meta.url));
const root = mkdtempSync(join(process.env.TEST_TMPDIR, 'sdk-orchestrator-'));
const input = '00000000-0000-4000-8000-0000000000a1';
const session = '00000000-0000-4000-8000-0000000000b2';
const profile = (resume) => ({
  resume,
  model: 'opus',
  tools: ['Skill', 'Read', 'Glob', 'Grep'],
  disallowed_tools: ['AskUserQuestion', 'ExitPlanMode'],
  mcp_servers: { 'alondra-orchestrator': { command: '/fixture/plugin/bin/alondra', args: ['mcp-orchestrator'] } },
  plugins: ['/fixture/plugin', '/fixture/plugin-orchestrator'],
  settings: { permissions: { additionalDirectories: ['/fixture/projects'], allow: ['Read(//fixture/projects/**)'], deny: ['Edit(//fixture/projects/**)'] } },
});
const value = (args, flag) => {
  const inline = args.find((arg) => arg.startsWith(`${flag}=`));
  if (inline) return inline.slice(flag.length + 1);
  const at = args.indexOf(flag);
  return at === -1 ? undefined : args[at + 1];
};

try {
  const { Session } = await import('./session.js');
  async function turn(name, orchestrator) {
    const home = join(root, name);
    mkdirSync(home);
    const capture = join(root, `${name}.json`);
    const observations = [];
    const run = new Session({
      executable: '/fixture/original-claude', cwd: root, sessionId: session, orchestrator,
      env: { HOME: home, PATH: '/fixture/no-js-runtime', FIXTURE_SCENARIO: 'success', FIXTURE_CAPTURE: capture },
      spawn: (options) => spawn(process.env.JS_BINARY__NODE_BINARY, [fixture, ...options.args], { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'inherit'] }),
      observe: (observation) => observations.push(observation),
    });
    run.offer(input, 'the batch');
    await run.finished;
    return { args: JSON.parse(readFileSync(capture)).args, observations, run };
  }

  const fresh = await turn('fresh', profile(false));
  assert.equal(fresh.observations.filter((o) => o.type === 'sdk' && o.kind === 'result').length, 1);
  assert(fresh.observations.some((o) => o.type === 'process_exited'), 'the turn ends the native process without being told to close');
  assert(!fresh.observations.some((o) => o.type === 'sdk' && o.kind === 'diagnostic'), JSON.stringify(fresh.observations));
  assert.throws(() => fresh.run.offer('00000000-0000-4000-8000-0000000000a2', 'a later batch'), /closed/, 'nothing joins a finished orchestrator turn');
  const args = fresh.args;
  assert.equal(value(args, '--model'), 'opus', args.join(' '));
  assert.equal(value(args, '--tools'), 'Skill,Read,Glob,Grep', args.join(' '));
  assert.equal(value(args, '--disallowedTools'), 'AskUserQuestion,ExitPlanMode', args.join(' '));
  assert.equal(value(args, '--permission-mode'), 'auto', args.join(' '));
  assert(args.includes('--strict-mcp-config'), args.join(' '));
  assert(args.includes('--restricted'), args.join(' '));
  assert.deepEqual(JSON.parse(value(args, '--mcp-config')).mcpServers['alondra-orchestrator'], { type: 'stdio', command: '/fixture/plugin/bin/alondra', args: ['mcp-orchestrator'] });
  assert.deepEqual(args.flatMap((arg, at) => arg.startsWith('--plugin-dir=') ? [arg.slice(13)] : args[at - 1] === '--plugin-dir' ? [arg] : []), ['/fixture/plugin', '/fixture/plugin-orchestrator'], args.join(' '));
  assert.deepEqual(JSON.parse(value(args, '--settings')).permissions.deny, ['Edit(//fixture/projects/**)']);
  assert.equal(value(args, '--session-id'), session);
  assert.equal(value(args, '--resume'), undefined);
  assert.equal(value(args, '--permission-prompt-tool'), undefined, 'nobody answers a prompt in an orchestrator turn');

  const resumed = await turn('resumed', profile(true));
  assert.equal(value(resumed.args, '--resume'), session, resumed.args.join(' '));
  assert.equal(value(resumed.args, '--session-id'), undefined, resumed.args.join(' '));
} finally {
  rmSync(root, { recursive: true, force: true });
}
