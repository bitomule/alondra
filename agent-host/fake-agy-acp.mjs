// A fake `agy` for the Antigravity host's tests, run by the test's own Node. It speaks ACP over
// stdio and records what it receives under ALONDRA_FAKE_AGY_RECORD. Each prompt follows the
// steps of ALONDRA_FAKE_AGY_SCRIPT (a JSON array with one array of steps per prompt); every wait
// is on a message from the host, never on a timer.
import { createHash } from 'node:crypto';
import { appendFileSync, chmodSync, mkdirSync, statSync, writeFileSync, writeSync } from 'node:fs';
import { join } from 'node:path';
import { createInterface } from 'node:readline';

const env = process.env;
const args = process.argv.slice(2);
const version = env.ALONDRA_FAKE_AGY_VERSION ?? '1.5.14';
const RECORDED_VALUES = ['TMPDIR', 'GEMINI_HOME', 'AGY_CLI_DISABLE_AUTO_UPDATE', 'AGY_ACP_FORCE_FILE_STORAGE', 'AGY_ADC_AUTH', 'SSH_CONNECTION', 'GOOGLE_CLOUD_QUOTA_PROJECT', 'GOOGLE_CLOUD_LOCATION', 'GOOGLE_APPLICATION_CREDENTIALS', 'ALONDRA_WAIT_FILE'];

function record(name, value) {
  if (env.ALONDRA_FAKE_AGY_RECORD) appendFileSync(join(env.ALONDRA_FAKE_AGY_RECORD, name), JSON.stringify(value) + '\n');
}
const matches = (name) => env[name] === undefined ? null : env[name] === env.ALONDRA_FAKE_AGY_EXPECTED_KEY;
const digest = (name) => env[name] === undefined ? null : createHash('sha256').update(env[name]).digest('hex');
let tmpdirMode = null;
try { if (env.TMPDIR) { const meta = statSync(env.TMPDIR); tmpdirMode = meta.isDirectory() ? meta.mode & 0o777 : null; } } catch { tmpdirMode = null; }
record('starts.jsonl', {
  argv: args, cwd: process.cwd(), pid: process.pid, names: Object.keys(env).sort(),
  values: Object.fromEntries(RECORDED_VALUES.filter((key) => key in env).map((key) => [key, env[key]])),
  geminiKeyMatches: matches('GEMINI_API_KEY'), googleKeyMatches: matches('GOOGLE_API_KEY'), tmpdirMode,
  // A digest, so a test checks which key arrived without the key reaching the record.
  geminiKeySha256: digest('GEMINI_API_KEY'), googleKeySha256: digest('GOOGLE_API_KEY'),
});
// A folder of agy's TMPDIR the host cannot empty, as one a tool left read-only.
if (env.ALONDRA_FAKE_AGY_LOCKED_TMPDIR === '1' && env.TMPDIR) {
  const locked = join(env.TMPDIR, 'locked');
  mkdirSync(locked, { recursive: true }); writeFileSync(join(locked, 'kept'), ''); chmodSync(locked, 0o500);
}
// Diagnostics go to stderr, which the host drains and never keeps.
process.stderr.write('fake agy diagnostics\n');

if (args[0] === '--version') { writeSync(1, `${version}\n`); process.exit(0); }
if (args[0] !== 'acp') { process.stderr.write('fake agy: unknown command\n'); process.exit(2); }

const send = (message) => writeSync(1, JSON.stringify({ jsonrpc: '2.0', ...message }) + '\n');
const reply = (id, result) => send({ id, result });
const refuse = (id, code, message, data) => send({ id, error: { code, message, ...(data ? { data } : {}) } });
const script = JSON.parse(env.ALONDRA_FAKE_AGY_SCRIPT ?? '[]');
const defaultTurn = [{ thought: 'Thinking about it' }, { message: 'Hello from Antigravity' }, { tool: 'echo hi' }, { end: 'end_turn' }];
const state = { model: 'gemini-4-carbon', level: 'medium', mode: 'default', session: env.ALONDRA_FAKE_AGY_SESSION_ID ?? 'fixture-acp-session-1', prompts: 0, requests: 0, tools: 0, ignoringTerm: false };
const MODELS = { 'gemini-4-carbon': 'Gemini 4 Carbon', 'gemini-4-neon': 'Gemini 4 Neon', 'fixture-model-extra': 'Fixture Extra' };
const LEVELS = { 'gemini-4-carbon': ['low', 'medium', 'high'], 'gemini-4-neon': ['low', 'medium', 'high', 'xhigh'], 'fixture-model-extra': ['low', 'medium', 'extra'] };
const MODES = (env.ALONDRA_FAKE_AGY_MODES ?? 'default,accept-edits,plan').split(',');
const configOptions = () => [
  { id: 'model', name: 'Model', category: 'model', type: 'select', currentValue: state.model, options: Object.entries(MODELS).map(([value, name]) => ({ value, name })) },
  { id: 'thought_level', name: 'Thinking', category: 'thought_level', type: 'select', currentValue: state.level, options: LEVELS[state.model].map((value) => ({ value, name: value === 'xhigh' ? 'Extra' : value })) },
  { id: 'mode', name: 'Mode', category: 'mode', type: 'select', currentValue: state.mode, options: MODES.map((value) => ({ value, name: value })) },
];
const opened = () => ({
  sessionId: state.session,
  models: { availableModels: Object.entries(MODELS).map(([modelId, name]) => ({ modelId, name })), currentModelId: state.model },
  modes: { currentModeId: state.mode, availableModes: MODES.map((id) => ({ id, name: id })) },
  configOptions: configOptions(),
});
const update = (sessionUpdate) => send({ method: 'session/update', params: { sessionId: state.session, update: sessionUpdate } });
const usage = { inputTokens: 120, outputTokens: 30, cachedReadTokens: 0, cachedWriteTokens: 0, thoughtTokens: 12, totalTokens: 162 };
const waiting = new Map();
let current;

function answerRequest(id) {
  return new Promise((resolve) => waiting.set(String(id), resolve));
}
function executeCall(command) {
  const toolCallId = `call_${++state.tools}`;
  return { sessionUpdate: 'tool_call', toolCallId, name: 'run_command', args: { CommandLine: command }, status: 'pending', kind: 'execute', title: command, rawInput: { CommandLine: command } };
}
async function runPrompt(id) {
  const turn = script[state.prompts] ?? defaultTurn;
  state.prompts += 1;
  if (current) record('overlap.jsonl', { id });
  let cancel;
  current = { id, cancelled: false, cancel: new Promise((resolve) => { cancel = resolve; }) };
  current.onCancel = () => { current.cancelled = true; cancel(); };
  const mine = current;
  const finish = (stopReason) => {
    if (current === mine) current = undefined;
    update({ sessionUpdate: 'usage_update', used: 162, size: 1048576 });
    reply(id, { stopReason, usage });
  };
  for (const step of turn) {
    if (step.thought) update({ sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: step.thought } });
    if (step.message) update({ sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: step.message } });
    if (step.tool) {
      const call = executeCall(step.tool);
      update(call);
      update({ sessionUpdate: 'tool_call_update', toolCallId: call.toolCallId, status: 'in_progress' });
      update({ sessionUpdate: 'tool_call_update', toolCallId: call.toolCallId, status: 'completed', rawOutput: { output: 'Command output:\nhi\n', exit_code: 0 }, content: [{ type: 'content', content: { type: 'text', text: 'Command output:\nhi\n' } }] });
    }
    // Ignoring SIGTERM, the fake stays alive until SIGKILL even once stdin closes.
    if (step.ignoreTerm) { state.ignoringTerm = true; process.on('SIGTERM', () => {}); setInterval(() => {}, 1 << 30); }
    if (step.permission) {
      const request = String(++state.requests);
      const options = step.permission.options ?? [{ optionId: 'allow', name: 'Allow', kind: 'allow_once' }, { optionId: 'deny', name: 'Deny', kind: 'reject_once' }];
      const call = { ...executeCall(step.permission.command ?? 'make test'), ...(step.permission.name ? { name: step.permission.name, title: step.permission.name } : {}) };
      update(call);
      const answered = answerRequest(request);
      send({ id: request, method: 'session/request_permission', params: { sessionId: state.session, toolCall: call, options } });
      if (step.permission.wait === false) continue;
      await answered;
      // agy asks again with the same id and the same question, and takes the answer it gets.
      if (step.permission.repeat) {
        const again = answerRequest(request);
        send({ id: request, method: 'session/request_permission', params: { sessionId: state.session, toolCall: call, options } });
        await again;
      }
      if (mine.cancelled) { finish('cancelled'); return; }
    }
    if (step.hold) {
      await mine.cancel;
      if (step.hold.ignoreCancel) return;
      finish('cancelled');
      return;
    }
    if (step.error) { if (current === mine) current = undefined; refuse(id, step.error.code, step.error.message ?? 'fixture error', step.error.data); return; }
    if (step.end) { finish(step.end); return; }
  }
  finish('end_turn');
}

function request(message) {
  const { id, method, params = {} } = message;
  if (env.ALONDRA_FAKE_AGY_SILENT_ON === method) return;
  if (env.ALONDRA_FAKE_AGY_EXIT_ON === method) process.exit(3);
  const signedOut = env.ALONDRA_FAKE_AGY_AUTH === 'signed_out';
  switch (method) {
    case 'initialize': {
      const sessionCapabilities = { ...(env.ALONDRA_FAKE_AGY_NO_RESUME === '1' ? {} : { resume: {} }), ...(env.ALONDRA_FAKE_AGY_CLOSE === '0' ? {} : { close: {} }), load: {}, list: {}, additionalDirectories: {} };
      reply(id, {
        protocolVersion: Number(env.ALONDRA_FAKE_AGY_PROTOCOL ?? 1),
        agentCapabilities: {
          loadSession: true, promptCapabilities: { image: true, audio: false, embeddedContext: true }, sessionCapabilities, auth: { logout: {} },
          _meta: { antigravity: { tier: true, ...(env.ALONDRA_FAKE_AGY_SYSTEM_PROMPT === '0' ? {} : { systemPrompt: { append: true } }) } },
        },
        agentInfo: { name: env.ALONDRA_FAKE_AGY_AGENT_NAME ?? 'Antigravity ACP Server', ...(version ? { version } : {}) },
        authMethods: [{ id: 'oauth-personal', name: 'Google' }, { id: 'oauth-business', name: 'Gemini Enterprise' }, { id: 'gemini-api-key', name: 'Gemini API key' }, { id: 'agent-platform', name: 'Agent Platform' }],
      });
      return;
    }
    case 'authenticate':
      // agy opens a browser sign-in for the OAuth methods and waits for it: never answered here.
      if (params.methodId === 'oauth-personal' || params.methodId === 'oauth-business') return;
      if ((params.methodId === 'gemini-api-key' && !env.GEMINI_API_KEY) || env.ALONDRA_FAKE_AGY_AUTH === 'bad_key') { refuse(id, -32602, 'Invalid params'); return; }
      reply(id, {});
      return;
    case 'session/new':
      if (signedOut) { refuse(id, -32000, 'Authentication required'); return; }
      reply(id, opened());
      return;
    case 'session/resume':
      if (signedOut) { refuse(id, -32000, 'Authentication required'); return; }
      if (env.ALONDRA_FAKE_AGY_RESUME === 'unknown') { refuse(id, -32603, 'Internal error'); return; }
      state.session = params.sessionId;
      reply(id, opened());
      return;
    case 'session/set_config_option':
      if (params.configId === 'model' && LEVELS[params.value]) { state.model = params.value; if (!LEVELS[state.model].includes(state.level)) state.level = 'medium'; }
      else if (params.configId === 'thought_level' && LEVELS[state.model].includes(params.value)) state.level = params.value;
      else { refuse(id, -32602, 'Invalid params'); return; }
      reply(id, { configOptions: configOptions() });
      return;
    case 'session/set_mode':
      if (!MODES.includes(params.modeId)) { refuse(id, -32602, 'Invalid params'); return; }
      state.mode = params.modeId;
      reply(id, {});
      return;
    case 'session/close':
      reply(id, {});
      return;
    case 'session/prompt':
      void runPrompt(id);
      return;
    default:
      refuse(id, -32601, 'Method not found');
  }
}

const input = createInterface({ input: process.stdin });
input.on('line', (line) => {
  if (!line.trim()) return;
  const message = JSON.parse(line);
  record('messages.jsonl', message);
  if (typeof message.method === 'string' && message.id !== undefined) request(message);
  else if (typeof message.method === 'string') { if (message.method === 'session/cancel') current?.onCancel(); }
  else waiting.get(String(message.id))?.(message);
});
input.on('close', () => { if (!state.ignoringTerm) process.exit(0); });
