import { createHash } from 'node:crypto';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { closeSync, constants, fstatSync, fsyncSync, lstatSync, mkdirSync, mkdtempSync, openSync, readFileSync, renameSync, rmSync, unlinkSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, isAbsolute, join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import type { Decision, DecisionKind } from './frame.js';
import { InputNotOffered, type DecisionRequest, type HostConfiguration, type HostSession } from './host-session.js';

export const ANTIGRAVITY_VERSION = 'antigravity-acp-1';
export const ANTIGRAVITY_CAPABILITIES = ['streaming_input', 'exact_decisions', 'owner_attach', 'commit_ack', 'interrupt', 'force_stop', 'set_model', 'set_permission_mode'] as const;
export const MINIMUM_AGY = '1.5.12';
const ACP_PROTOCOL = 1;
const MAX_NATIVE_LINE = 4 * 1024 * 1024;
const MAX_PENDING = 64;
const MAX_QUEUED = 64;
/** A request belongs to the turn that asked it: its answer is kept to repeat until the turn ends, the oldest dropped past this many. */
const MAX_ANSWERED = 256;
const MAX_PROMPT_BYTES = 900_000;
/** The largest key file read, and the longest key it may hold: the daemon's `KEY_FILE_LIMIT` and `KEY_LIMIT`. */
const MAX_KEY_FILE_BYTES = 4096;
const MAX_KEY_BYTES = 512;
/** agy answers its startup in a few seconds; past this its session is not going to open. */
const OPENING_TIMEOUT_MS = 90_000;
/** How long `session/close` may take before the process group is ended anyway. */
const CLOSE_WAIT_MS = 500;
const STOP_ESCALATION_MS = 1000;
/** How long agy may take to exit after closing its output before the closed output is the failure. */
const OUTPUT_EXIT_GRACE_MS = 1000;
/** JSON-RPC code agy answers a session request with when the account is not signed in. */
const AUTHENTICATION_REQUIRED = -32000;
/** JSON-RPC code agy answers `authenticate` with when the key it was given is missing or unusable. */
const CREDENTIAL_INVALID = -32602;
/** The access types agy signs in by itself: `authenticate` would open a browser sign-in and wait for it. */
const AUTHENTICATES_ITSELF: ReadonlySet<Access> = new Set(['google', 'enterprise']);
const ACCESS_METHODS = { google: 'oauth-personal', enterprise: 'oauth-business', api_key: 'gemini-api-key', cloud_project: 'agent-platform' } as const;
type Access = keyof typeof ACCESS_METHODS;
/** How a tool of Alondra's MCP server is named: `mcp_<server>_<tool>`. */
const ALONDRA_TOOL_PREFIX = 'mcp_alondra_';
const NEVER_CHOSEN = new Set(['allow_always', 'reject_always']);
const EFFORT_LEVELS: Record<string, readonly string[]> = { low: ['low'], medium: ['medium'], high: ['high'], xhigh: ['xhigh', 'extra'], max: ['max'] };
/** Credentials, and where agy keeps a sign-in, that agy would otherwise inherit: a node only ever runs on its own account's. The daemon's `CREDENTIAL_VARIABLES`. */
export const CREDENTIAL_ENVIRONMENT = ['GEMINI_API_KEY', 'GOOGLE_API_KEY', 'GOOGLE_APPLICATION_CREDENTIALS', 'GOOGLE_CLOUD_PROJECT', 'GOOGLE_CLOUD_LOCATION', 'GOOGLE_CLOUD_QUOTA_PROJECT', 'GOOGLE_GENAI_USE_VERTEXAI', 'AGY_ADC_AUTH', 'GOOGLE_GEMINI_BASE_URL', 'GEMINI_HOME', 'AGY_ACP_FORCE_FILE_STORAGE', 'SSH_CONNECTION'] as const;
/** agy 1.5 keeps an OAuth sign-in in its folder only when it believes it runs remotely. */
const FILE_STORAGE_SSH_CONNECTION = '127.0.0.1 0 127.0.0.1 0';
type OpeningStep = 'initialize' | 'authenticate' | 'session_resume' | 'session_new' | 'configure';
type Mode = 'default' | 'plan' | 'dontAsk';
type Json = Record<string, unknown>;
type RequestId = string | number;
type Pending = { method: string; resolve: (value: Json) => void; reject: (error: Error) => void };
type Option = { optionId: string; name: string; kind: string };
type Asked = { id: RequestId; epoch: number; kind: DecisionKind; payload: Json; tool: string; options: Option[]; identity: string };
type Turn = { id: string; inputId: string; cancelled: boolean };
type Mapping = { acpSessionId: string; account: unknown; access: unknown };
const object = (value: unknown): Json => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Json : {};
const isObject = (value: unknown): value is Json => value !== null && typeof value === 'object' && !Array.isArray(value);
const text = (value: unknown): string | undefined => typeof value === 'string' && value.length > 0 ? value : undefined;
const requestKey = (id: RequestId): string => `${typeof id}:${id}`;
const settingsOf = (config: HostConfiguration): Json => object(config.worker?.settings.antigravity);

/** A refusal before the session opened, with the failure code the provider reports. */
class OpeningRefused extends Error {
  constructor(readonly code: string) { super(code); }
}
/** The account cannot run a session: nothing was offered to agy. */
class AccountRefused extends InputNotOffered {}
/** agy answered a request with a JSON-RPC error. */
class RpcRefused extends Error {
  constructor(readonly code: number, readonly data?: Json) { super(`Antigravity RPC ${code}`); }
}

/** A leading `MAJOR.MINOR.PATCH`, nothing else read from it. */
export function parseAgyVersion(value: unknown): { text: string; parts: [number, number, number] } | undefined {
  if (typeof value !== 'string') return undefined;
  const match = /^(0|[1-9]\d{0,8})\.(0|[1-9]\d{0,8})\.(0|[1-9]\d{0,8})(?=$|\s)/.exec(value);
  if (!match) return undefined;
  return { text: `${match[1]}.${match[2]}.${match[3]}`, parts: [Number(match[1]), Number(match[2]), Number(match[3])] };
}
export function agyVersionSupported(parts: readonly [number, number, number]): boolean {
  const minimum = parseAgyVersion(MINIMUM_AGY)!.parts;
  for (let at = 0; at < 3; at++) if (parts[at] !== minimum[at]) return parts[at]! > minimum[at]!;
  return true;
}

const ROLE_ENVIRONMENT = ['ALONDRA_HOME', 'ALONDRA_DOMAIN_CAPABILITY', 'ALONDRA_DOMAIN_PROTOCOL', 'ALONDRA_SESSION_ID', 'ALONDRA_ORCHESTRATOR', 'ALONDRA_ORCHESTRATOR_EXECUTION', 'ALONDRA_PORT', 'ALONDRA_SANDBOX', 'ALONDRA_BIN', 'ALONDRA_PROVIDER'] as const;
export function alondraMcpEnvironment(config: HostConfiguration): Record<string, string> {
  if (!config.worker) return {};
  const env = Object.fromEntries(ROLE_ENVIRONMENT.flatMap((key) => typeof config.env[key] === 'string' ? [[key, config.env[key] as string]] : []));
  for (const key of ['ALONDRA_HOME', 'ALONDRA_DOMAIN_CAPABILITY', 'ALONDRA_DOMAIN_PROTOCOL', 'ALONDRA_SESSION_ID', 'ALONDRA_PROVIDER']) if (!env[key]) throw new Error(`Alondra role environment is missing ${key}`);
  if (!isAbsolute(env.ALONDRA_HOME ?? '') || env.ALONDRA_DOMAIN_PROTOCOL !== '2' || env.ALONDRA_PROVIDER !== 'antigravity') throw new Error('Alondra role environment is invalid');
  if (env.ALONDRA_PORT && (!/^\d+$/.test(env.ALONDRA_PORT) || Number(env.ALONDRA_PORT) < 1 || Number(env.ALONDRA_PORT) > 65535)) throw new Error('Alondra role port is invalid');
  if (env.ALONDRA_ORCHESTRATOR === '1') throw new Error('An Alondra node cannot inherit orchestrator identity');
  return env;
}

/**
 * The key an account keeps for agy, by the daemon's rule (`key_in_file`, `key_file_private`): a
 * regular file of the user's that no one else may read or write, with no other link, read without
 * following links; the key is its text without the white space around it.
 */
export function readKeyFile(path: string): string {
  if (!isAbsolute(path)) throw new OpeningRefused('native_initialization_failed');
  let fd: number;
  try { fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK); }
  catch (error) { throw (error as NodeJS.ErrnoException).code === 'ENOENT' ? new AccountRefused('authentication_required') : new OpeningRefused('native_initialization_failed'); }
  try {
    const meta = fstatSync(fd);
    if (!meta.isFile() || meta.nlink !== 1 || meta.uid !== process.getuid?.() || (meta.mode & 0o077) !== 0 || meta.size > MAX_KEY_FILE_BYTES) throw new OpeningRefused('native_initialization_failed');
    const bytes = readFileSync(fd);
    let text: string;
    try { text = new TextDecoder('utf-8', { fatal: true }).decode(bytes); }
    catch { throw new OpeningRefused('native_initialization_failed'); }
    finally { bytes.fill(0); }
    const key = text.replace(/^\p{White_Space}+|\p{White_Space}+$/gu, '');
    if (!key) throw new AccountRefused('authentication_required');
    if (Buffer.byteLength(key) > MAX_KEY_BYTES || /[\p{White_Space}\p{Cc}]/u.test(key)) throw new OpeningRefused('native_initialization_failed');
    return key;
  } finally { closeSync(fd); }
}

function accessOf(settings: Json): Access {
  const access = settings.access;
  if (typeof access !== 'string' || !Object.hasOwn(ACCESS_METHODS, access)) throw new OpeningRefused('native_initialization_failed');
  return access as Access;
}
function absolutePath(value: unknown): string | undefined {
  if (value === undefined) return undefined;
  if (typeof value !== 'string' || !isAbsolute(value) || value.includes('\0')) throw new OpeningRefused('native_initialization_failed');
  return value;
}

/** The environment of the account's `agy`: the inherited one without any Google credential, plus the account's own. */
export function antigravityEnvironment(config: HostConfiguration, temporary: string): Record<string, string> {
  const settings = settingsOf(config);
  const access = accessOf(settings);
  const env: Record<string, string> = {};
  for (const [key, value] of Object.entries(config.env)) if (typeof value === 'string') env[key] = value;
  for (const key of CREDENTIAL_ENVIRONMENT) delete env[key];
  if (config.stateDirectory) env.ALONDRA_WAIT_FILE = join(config.stateDirectory, 'wait.json');
  env.AGY_CLI_DISABLE_AUTO_UPDATE = '1';
  env.TMPDIR = temporary;
  const folder = absolutePath(settings.account_folder);
  if (folder) {
    env.GEMINI_HOME = folder;
    if (access === 'google' || access === 'enterprise') {
      env.AGY_ACP_FORCE_FILE_STORAGE = '1';
      env.SSH_CONNECTION = FILE_STORAGE_SSH_CONNECTION;
    }
  }
  const project = text(settings.project); const location = text(settings.location);
  const place = () => {
    if (project) env.GOOGLE_CLOUD_QUOTA_PROJECT = project;
    if (location) env.GOOGLE_CLOUD_LOCATION = location;
  };
  const keyFile = absolutePath(settings.key_file);
  switch (access) {
    case 'api_key':
      if (!keyFile) throw new AccountRefused('authentication_required');
      env.GEMINI_API_KEY = readKeyFile(keyFile);
      break;
    case 'enterprise':
      place();
      break;
    case 'cloud_project':
      if (keyFile) env.GOOGLE_API_KEY = readKeyFile(keyFile);
      else {
        env.AGY_ADC_AUTH = 'true';
        const credentials = absolutePath(settings.credentials_file);
        if (credentials) env.GOOGLE_APPLICATION_CREDENTIALS = credentials;
      }
      place();
      break;
    case 'google':
      break;
  }
  return env;
}

/** A private folder of this host's own for agy's `TMPDIR`: each start unpacks its runtime there. */
function privateTemporary(config: HostConfiguration): string {
  const folder = config.stateDirectory ? mkdtempSync(join(config.stateDirectory, 'native-tmp-')) : mkdtempSync(join(tmpdir(), 'alondra-agy-'));
  const meta = lstatSync(folder);
  if (!meta.isDirectory() || meta.isSymbolicLink() || meta.uid !== process.getuid?.() || (meta.mode & 0o777) !== 0o700) {
    rmSync(folder, { recursive: true, force: true });
    throw new Error('Private temporary folder refused');
  }
  return folder;
}

function optionValues(option: Json | undefined): string[] {
  const values: string[] = [];
  const visit = (entries: unknown) => {
    for (const entry of Array.isArray(entries) ? entries : []) {
      const value = object(entry);
      if (typeof value.value === 'string') values.push(value.value);
      else if (Array.isArray(value.options)) visit(value.options);
    }
  };
  visit(option?.options);
  return values;
}
/** The tool's own name, never its title: agy titles a command with its command line, which the model writes. */
function namesAlondra(toolCall: Json): boolean {
  return typeof toolCall.name === 'string' && toolCall.name.startsWith(ALONDRA_TOOL_PREFIX);
}

export type Launcher = { argvPrefix?: readonly string[] };

/** One `agy acp` process for one node, in the host's own process group. Owns its stdio for its whole lifetime. */
export class AntigravitySession implements HostSession {
  readonly finished: Promise<void>;
  /** Why the native session ended without being asked to. */
  failure?: string;
  child?: ChildProcessWithoutNullStreams;
  private finish!: () => void;
  private ready: Promise<void>;
  private pending = new Map<string, Pending>();
  private asked = new Map<string, Asked>();
  private answered = new Map<string, { identity: string; result: Json }>();
  private sequence = 0;
  private buffer = Buffer.alloc(0);
  private held: Json[] = [];
  private heldBytes = 0;
  private inputs = new Set<string>();
  private queue: { id: string; prompt: string }[] = [];
  private preamble?: string;
  private unsaved?: { account: string; access: Access };
  private native?: string;
  private opened = false;
  private turn?: Turn;
  private starting = false;
  private pumping = false;
  private closing = false;
  private closed = false;
  private stopDeadline?: ReturnType<typeof setTimeout>;
  private outputDeadline?: ReturnType<typeof setTimeout>;
  private openingDeadline?: ReturnType<typeof setTimeout>;
  private opening: OpeningStep = 'initialize';
  private mode: Mode = 'default';
  private acpMode?: string;
  private modes: string[] = [];
  private offersModes = false;
  private configOptions: Json[] = [];
  private model?: string;
  private appendsSystemPrompt = false;
  private closeAdvertised = false;
  private agyVersion?: string;
  private mapping?: string;
  private temporary?: string;
  private argvPrefix: string[];

  constructor(private config: HostConfiguration, launcher: Launcher = {}) {
    if (!config.sessionId || !/^[0-9a-f-]{36}$/.test(config.sessionId)) throw new Error('Antigravity conversation identity absent');
    if (config.orchestrator) throw new Error('Antigravity runs Alondra nodes only');
    alondraMcpEnvironment(config);
    this.argvPrefix = [...(launcher.argvPrefix ?? [])];
    this.finished = new Promise((resolve) => { this.finish = resolve; });
    this.model = config.worker?.model;
    const initial = settingsOf(config).initial_mode;
    if (initial === 'plan' || initial === 'dontAsk') this.mode = initial;
    this.config.beforeSpawn?.();
    this.temporary = privateTemporary(config);
    this.ready = this.open();
    this.openingDeadline = setTimeout(() => { if (!this.opened) this.fail('native_initialization_timeout'); }, config.openingTimeoutMs ?? OPENING_TIMEOUT_MS);
    this.openingDeadline.unref();
    void this.ready.catch((error: unknown) => this.refused(error));
  }

  private emit(method: string, params: Json, kind = 'message', turn?: Turn): number | void {
    return this.config.observe({ type: 'sdk', kind, native: turn ? { turn_id: turn.id } : {}, payload: { method, params }, failures: [] });
  }
  private fail(code: string): void {
    if (this.closed || this.closing) return;
    this.failure = code;
    this.close();
  }
  private refused(error: unknown): void {
    if (this.closed || this.closing) return;
    const code = error instanceof AccountRefused || error instanceof OpeningRefused ? error.code : 'native_initialization_failed';
    if (error instanceof AccountRefused) {
      this.refuseQueued(code);
      this.emit('alondra/accountBlocked', { sessionId: null, code }, 'diagnostic');
    }
    this.fail(code);
  }
  private refuseQueued(code: string): void {
    for (const input of this.queue.splice(0)) this.emit('alondra/inputRefused', { sessionId: this.native ?? null, inputId: input.id, nativeSubmitted: false, code }, 'diagnostic');
  }

  private spawnNative(env: Record<string, string>, args: string[]): void {
    let child: ChildProcessWithoutNullStreams;
    try {
      child = spawn(this.config.executable, [...this.argvPrefix, ...args], { cwd: this.config.cwd, env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true });
    } catch { throw new OpeningRefused('native_spawn_failed'); }
    this.child = child;
    child.stdout.on('data', (bytes: Buffer) => this.consume(bytes));
    // agy's stderr must always be drained. It is not evidence and may carry credentials.
    child.stderr.on('data', () => {});
    child.on('error', () => this.fail('native_spawn_failed'));
    let exiting = false;
    const ended = (code: number | null, signal: NodeJS.Signals | null) => {
      if (exiting || this.closed) return;
      exiting = true;
      clearTimeout(this.stopDeadline);
      clearTimeout(this.outputDeadline);
      clearTimeout(this.openingDeadline);
      if (!this.closing) this.failure ??= 'native_process_exited';
      this.closing = true;
      let drainDeadline: ReturnType<typeof setTimeout> | undefined;
      let finishing = false;
      const finish = () => {
        if (finishing) return;
        finishing = true;
        clearTimeout(drainDeadline);
        queueMicrotask(() => {
          child.stdout.destroy();
          child.stderr.destroy();
          this.end(code, signal);
        });
      };
      // Exit precedes pipe EOF: keep agy's last evidence, but a descendant holding the pipe
      // cannot keep a dead session open.
      drainDeadline = setTimeout(finish, 1000);
      if (child.stdout.readableEnded || child.stdout.destroyed) finish();
      else {
        child.stdout.once('end', finish);
        child.stdout.once('close', finish);
        child.stdout.resume();
      }
    };
    child.on('exit', ended);
    child.on('close', ended);
    child.stdin.on('error', () => this.fail('native_input_closed'));
    child.stdin.on('close', () => this.fail('native_input_closed'));
    // EOF usually means agy is exiting: its exit names the failure, unless it stays alive.
    const outputClosed = () => {
      if (this.closing || this.closed || this.outputDeadline) return;
      this.outputDeadline = setTimeout(() => this.fail('native_output_closed'), OUTPUT_EXIT_GRACE_MS);
      this.outputDeadline.unref();
    };
    child.stdout.on('end', outputClosed);
    child.stdout.on('close', outputClosed);
    child.stdout.on('error', () => this.fail('native_output_closed'));
    child.stderr.on('error', () => {});
  }
  private signal(name: NodeJS.Signals): void {
    try { this.child?.kill(name); } catch { /* agy has exited. */ }
  }
  private end(code: number | null, signal: string | null): void {
    if (this.closed) return;
    this.closed = true;
    this.rejectPending();
    for (const [key, entry] of this.asked) this.settled(key, entry, 'cancelled');
    this.refuseQueued(this.failure ?? 'native_session_ended');
    if (this.failure) this.emit('alondra/transportFailed', { code: this.failure, ...(this.failure === 'native_initialization_timeout' ? { step: this.opening } : {}) }, 'diagnostic');
    this.config.observe({ type: 'process_exited', exit_code: code, signal });
    // A folder agy's surviving children still write to, or one left unwritable, never keeps the end from being reported.
    try { if (this.temporary) rmSync(this.temporary, { recursive: true, force: true }); } catch {}
    this.finish();
  }
  private rejectPending(): void {
    for (const pending of this.pending.values()) pending.reject(new Error('Antigravity process ended'));
    this.pending.clear();
  }
  private send(frame: Json): Promise<void> {
    const child = this.child;
    if (this.closed || !child || child.stdin.destroyed) return Promise.reject(new Error('Antigravity pipe closed'));
    const bytes = JSON.stringify({ jsonrpc: '2.0', ...frame }) + '\n';
    if (Buffer.byteLength(bytes) > MAX_NATIVE_LINE) return Promise.reject(new Error('Antigravity request too large'));
    return new Promise((resolve, reject) => child.stdin.write(bytes, (error) => error ? reject(error) : resolve()));
  }
  private rpc(method: string, params: Json): Promise<Json> {
    if (this.pending.size >= MAX_PENDING) return Promise.reject(new Error('Antigravity request capacity'));
    const id = `alondra:${++this.sequence}`;
    return new Promise((resolve, reject) => {
      this.pending.set(requestKey(id), { method, resolve, reject });
      void this.send({ id, method, params }).catch((error: Error) => { this.pending.delete(requestKey(id)); reject(error); });
    });
  }
  private consume(bytes: Buffer): void {
    try {
      this.buffer = Buffer.concat([this.buffer, bytes]);
      for (;;) {
        const newline = this.buffer.indexOf(10);
        if (newline < 0) { if (this.buffer.length > MAX_NATIVE_LINE) throw new Error('Native line capacity'); return; }
        if (newline > MAX_NATIVE_LINE) throw new Error('Native line capacity');
        const line = this.buffer.subarray(0, newline); this.buffer = this.buffer.subarray(newline + 1);
        if (!line.length) continue;
        const message: unknown = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line));
        if (!isObject(message)) throw new Error('Invalid native message');
        if (this.starting) {
          // A prompt's answer and updates are evidence only after the prompt is known to be sent.
          this.heldBytes += line.length;
          if (this.held.length >= 4096 || this.heldBytes > 16 * MAX_NATIVE_LINE) throw new Error('Native message capacity');
          this.held.push(message);
        } else this.handle(message);
      }
    } catch { this.fail('native_protocol_failed'); }
  }
  private release(): void {
    try { for (const message of this.held.splice(0)) this.handle(message); }
    catch { this.fail('native_protocol_failed'); }
    finally { this.heldBytes = 0; }
  }
  private handle(message: Json): void {
    const id = message.id;
    const identified = typeof id === 'string' || typeof id === 'number';
    if (typeof message.method === 'string') {
      if (identified) this.request(message);
      else if (message.method === 'session/update') this.config.observe({ type: 'sdk', kind: 'message', native: this.turn ? { turn_id: this.turn.id } : {}, payload: message, failures: [] });
      return;
    }
    if (!identified) throw new Error('Invalid native message');
    const key = requestKey(id); const pending = this.pending.get(key);
    if (!pending) return;
    this.pending.delete(key);
    if (message.error !== undefined) {
      const error = object(message.error);
      if (!Number.isInteger(error.code)) throw new Error('Invalid native error');
      pending.reject(new RpcRefused(error.code as number, isObject(error.data) ? error.data : undefined));
    } else pending.resolve(object(message.result));
  }

  private async open(): Promise<void> {
    const settings = settingsOf(this.config);
    const access = accessOf(settings);
    const env = antigravityEnvironment(this.config, this.temporary!);
    const folder = absolutePath(settings.account_folder);
    if (this.closing) throw new Error('Antigravity session closed');
    this.spawnNative(env, folder ? ['acp', '--gemini-dir', folder] : ['acp']);
    this.opening = 'initialize';
    const initialized = await this.rpc('initialize', {
      protocolVersion: ACP_PROTOCOL,
      clientCapabilities: { fs: { readTextFile: false, writeTextFile: false }, terminal: false },
      clientInfo: { name: 'alondra', title: 'Alondra', version: this.config.hostRelease ?? 'development' },
    });
    this.checkAgent(initialized);
    if (!AUTHENTICATES_ITSELF.has(access)) {
      this.opening = 'authenticate';
      try { await this.rpc('authenticate', { methodId: ACCESS_METHODS[access] }); }
      catch (error) {
        if (error instanceof RpcRefused && (error.code === AUTHENTICATION_REQUIRED || error.code === CREDENTIAL_INVALID)) throw new AccountRefused('authentication_required');
        throw error;
      }
    }
    const mcpServers = this.mcpServers(settings);
    const instructions = text(settings.instructions);
    const stored = this.readMapping();
    const account = this.config.accountId ?? 'default';
    let opened: Json | undefined;
    let id: string | undefined;
    if (stored && stored.account === account && stored.access === access) {
      this.opening = 'session_resume';
      try {
        opened = await this.rpc('session/resume', { sessionId: stored.acpSessionId, cwd: this.config.cwd, mcpServers });
        id = stored.acpSessionId;
      } catch (error) {
        // Any other refusal leaves the stored session behind: agy answers an unknown one with a generic error.
        if (!(error instanceof RpcRefused)) throw error;
        if (error.code === AUTHENTICATION_REQUIRED) throw new AccountRefused('authentication_required');
      }
    }
    const resumed = id !== undefined;
    if (!resumed) {
      this.opening = 'session_new';
      const meta = instructions && this.appendsSystemPrompt ? { _meta: { antigravity: { systemPrompt: { append: instructions } } } } : {};
      try { opened = await this.rpc('session/new', { cwd: this.config.cwd, mcpServers, ...meta }); }
      catch (error) {
        if (error instanceof RpcRefused && error.code === AUTHENTICATION_REQUIRED) throw new AccountRefused('authentication_required');
        throw error;
      }
      id = text(opened.sessionId);
      if (!id || id.length > 256) throw new Error('Antigravity session identity absent');
      // The daemon hands a first launch its context; a session that could not be resumed needs it here.
      const handoff = stored || this.config.resume ? text(this.config.worker?.handoff_context) : undefined;
      const preamble = [instructions && !this.appendsSystemPrompt ? instructions : undefined, handoff].filter((part): part is string => part !== undefined);
      if (preamble.length) this.preamble = preamble.join('\n\n');
    }
    this.native = id!;
    // A new session that still owes the node its instructions or handoff is kept for a later host only
    // once the prompt carrying them is sent: resuming it earlier would lose them.
    if (this.preamble) this.unsaved = { account, access };
    else this.writeMapping(this.native, account, access);
    this.opening = 'configure';
    const result = object(opened);
    if (Array.isArray(result.configOptions)) this.configOptions = result.configOptions.map(object);
    const modes = result.modes;
    if (isObject(modes)) {
      this.offersModes = true;
      this.acpMode = text(modes.currentModeId);
      this.modes = (Array.isArray(modes.availableModes) ? modes.availableModes : []).flatMap((mode) => text(object(mode).id) ?? []);
    }
    if (this.model && this.option('model') && this.option('model')!.currentValue !== this.model) await this.setOption('model', this.model);
    await this.applyEffort();
    await this.applyMode();
    this.model = text(this.option('model')?.currentValue) ?? this.model;
    if (this.closing) throw new Error('Antigravity session closed');
    this.opened = true;
    clearTimeout(this.openingDeadline);
    this.emit('alondra/sessionReady', {
      sessionId: this.native, logicalConversationId: this.config.sessionId, cwd: this.config.cwd, ...(this.model ? { model: this.model } : {}),
      processGeneration: this.config.processGeneration ?? this.config.sessionId, resumed, agyVersion: this.agyVersion,
    }, 'initialized');
    void this.pump();
  }
  private checkAgent(initialized: Json): void {
    const info = object(initialized.agentInfo);
    const capabilities = object(initialized.agentCapabilities);
    const sessions = object(capabilities.sessionCapabilities);
    if (initialized.protocolVersion !== ACP_PROTOCOL || !text(info.name)?.toLowerCase().includes('antigravity')) throw new OpeningRefused('native_incompatible');
    const version = parseAgyVersion(info.version);
    if (!version) throw new OpeningRefused('native_version_unreadable');
    if (!agyVersionSupported(version.parts)) throw new OpeningRefused('native_version_unsupported');
    if (sessions.resume === undefined || sessions.resume === null) throw new OpeningRefused('native_incompatible');
    this.agyVersion = version.text;
    this.closeAdvertised = sessions.close !== undefined && sessions.close !== null;
    this.appendsSystemPrompt = Boolean(object(object(object(capabilities._meta).antigravity).systemPrompt).append);
  }
  private mcpServers(settings: Json): Json[] {
    if (!this.config.worker) return [];
    const server = object(object(settings.mcp_servers).alondra);
    const command = text(server.command);
    if (!command || !isAbsolute(command)) throw new OpeningRefused('alondra_mcp_missing');
    const args = Array.isArray(server.args) ? server.args : [];
    if (args.some((arg) => typeof arg !== 'string')) throw new OpeningRefused('alondra_mcp_missing');
    const env = alondraMcpEnvironment(this.config);
    return [{ name: 'alondra', command, args, env: Object.entries(env).map(([name, value]) => ({ name, value })) }];
  }
  private option(kind: 'model' | 'thought_level'): Json | undefined {
    return kind === 'model' ? this.configOptions.find((option) => option.id === 'model' || option.category === 'model') :
      this.configOptions.find((option) => option.category === 'thought_level');
  }
  private async setOption(kind: 'model' | 'thought_level', value: string): Promise<void> {
    const option = this.option(kind);
    const configId = text(option?.id);
    if (!configId) throw new Error(`Antigravity offers no ${kind} option`);
    const result = await this.rpc('session/set_config_option', { sessionId: this.native, configId, value });
    if (Array.isArray(result.configOptions)) this.configOptions = result.configOptions.map(object);
    else option!.currentValue = value;
  }
  private async applyEffort(): Promise<void> {
    const effort = this.config.worker?.effort;
    const level = this.option('thought_level');
    if (!effort || !level) return;
    const offered = optionValues(level);
    const chosen = (EFFORT_LEVELS[effort] ?? [effort]).find((candidate) => offered.includes(candidate));
    if (chosen && level.currentValue !== chosen) await this.setOption('thought_level', chosen);
  }
  private acpModeFor(mode: Mode): string {
    return mode === 'plan' ? 'plan' : mode === 'default' && this.modes.includes('auto') ? 'auto' : 'default';
  }
  private async applyMode(): Promise<void> {
    if (!this.offersModes) return;
    const target = this.acpModeFor(this.mode);
    if (target === this.acpMode) return;
    await this.rpc('session/set_mode', { sessionId: this.native, modeId: target });
    this.acpMode = target;
  }

  private readMapping(): Mapping | undefined {
    if (!this.config.stateDirectory) return undefined;
    const directory = join(dirname(this.config.stateDirectory), 'native-sessions');
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const meta = lstatSync(directory);
    if (!meta.isDirectory() || meta.isSymbolicLink() || meta.uid !== process.getuid?.() || (meta.mode & 0o777) !== 0o700) throw new Error('Private session directory refused');
    this.mapping = join(directory, this.config.sessionId!);
    let fd: number;
    try { fd = openSync(this.mapping, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined; throw new Error('Private native session refused'); }
    try {
      const node = fstatSync(fd);
      if (!node.isFile() || node.nlink !== 1 || node.uid !== process.getuid?.() || (node.mode & 0o777) !== 0o600 || node.size > 4096) throw new Error('Private native session refused');
      const retained = object(JSON.parse(readFileSync(fd, 'utf8')));
      const acpSessionId = text(retained.acpSessionId);
      if (!acpSessionId || acpSessionId.length > 256) throw new Error('Native session identity refused');
      return { acpSessionId, account: retained.account, access: retained.access };
    } finally { closeSync(fd); }
  }
  private writeMapping(acpSessionId: string, account: string, access: Access): void {
    if (!this.mapping) return;
    const next = this.mapping + '.next';
    try { unlinkSync(next); } catch (error) { if ((error as NodeJS.ErrnoException).code !== 'ENOENT') throw error; }
    const fd = openSync(next, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
    try { writeFileSync(fd, JSON.stringify({ acpSessionId, account, access, agyVersion: this.agyVersion })); fsyncSync(fd); } finally { closeSync(fd); }
    renameSync(next, this.mapping);
    const directory = openSync(dirname(this.mapping), constants.O_RDONLY);
    try { fsyncSync(directory); } finally { closeSync(directory); }
  }

  offer(inputId: string, prompt: string): void | Promise<void> {
    if (this.closed || this.closing) throw new InputNotOffered(this.failure ?? 'native_session_ended');
    if (this.inputs.has(inputId) || this.queue.length >= MAX_QUEUED) throw new Error('Antigravity input refused');
    if (Buffer.byteLength(prompt) > MAX_PROMPT_BYTES) throw new Error('Antigravity input capacity');
    this.inputs.add(inputId); this.queue.push({ id: inputId, prompt });
    void this.pump();
    if (!this.opened) return this.ready.then(() => undefined, (error: unknown) => {
      throw new InputNotOffered(this.failure ?? (error instanceof InputNotOffered ? error.code : 'native_initialization_failed'));
    });
  }
  /** One prompt at a time: an input offered during a turn waits until agy answers the prompt. */
  private async pump(): Promise<void> {
    if (this.pumping || this.closed || this.closing || this.turn || !this.opened || this.queue.length === 0) return;
    this.pumping = true;
    try {
      const input = this.queue.shift()!;
      const prompt = this.preamble ? `${this.preamble}\n\n${input.prompt}` : input.prompt;
      this.preamble = undefined;
      const id = `alondra:${++this.sequence}`;
      const turn: Turn = { id, inputId: input.id, cancelled: false };
      this.turn = turn;
      this.pending.set(requestKey(id), {
        method: 'session/prompt',
        resolve: (result) => this.promptEnded(turn, result),
        reject: (error) => { if (error instanceof RpcRefused) this.promptFailed(turn, error); },
      });
      this.starting = true;
      try {
        await this.send({ id, method: 'session/prompt', params: { sessionId: this.native, prompt: [{ type: 'text', text: prompt }] } });
      } catch {
        this.starting = false;
        this.pending.delete(requestKey(id));
        this.fail('native_prompt_uncertain');
        return;
      }
      if (this.unsaved) {
        const { account, access } = this.unsaved;
        this.unsaved = undefined;
        try { this.writeMapping(this.native!, account, access); }
        catch { this.starting = false; this.fail('native_session_not_durable'); return; }
      }
      this.emit('alondra/promptSent', { sessionId: this.native, inputId: input.id, turnId: id }, 'message', turn);
      this.starting = false;
      this.release();
    } finally { this.pumping = false; }
  }
  private promptEnded(turn: Turn, result: Json): void {
    this.emit('alondra/promptEnded', { sessionId: this.native, inputId: turn.inputId, turnId: turn.id, stopReason: typeof result.stopReason === 'string' ? result.stopReason : null }, 'message', turn);
    this.endTurn(turn);
  }
  private promptFailed(turn: Turn, error: RpcRefused): void {
    this.emit('alondra/promptFailed', { sessionId: this.native, inputId: turn.inputId, turnId: turn.id, error: { code: error.code, ...(error.data ? { data: error.data } : {}) } }, 'message', turn);
    this.endTurn(turn);
  }
  private endTurn(turn: Turn): void {
    if (this.turn !== turn) return;
    this.cancelAsked();
    this.answered.clear();
    this.turn = undefined;
    void this.pump();
  }

  private request(message: Json): void {
    const id = message.id as RequestId;
    if (message.method !== 'session/request_permission') {
      void this.send({ id, error: { code: -32601, message: 'Alondra does not offer this method' } }).catch(() => this.fail('native_response_uncertain'));
      return;
    }
    const params = object(message.params);
    const key = requestKey(id);
    const identity = createHash('sha256').update(JSON.stringify(params)).digest('hex');
    const answered = this.answered.get(key);
    if (answered) {
      if (answered.identity !== identity) { this.fail('native_request_identity_conflict'); return; }
      void this.send({ id, result: answered.result }).catch(() => this.fail('native_response_uncertain'));
      return;
    }
    const previous = this.asked.get(key);
    if (previous) {
      if (previous.identity !== identity) this.fail('native_request_identity_conflict');
      return;
    }
    const toolCall = object(params.toolCall);
    const options: Option[] = (Array.isArray(params.options) ? params.options : []).flatMap((entry) => {
      const option = object(entry);
      return typeof option.optionId === 'string' && typeof option.kind === 'string' ? [{ optionId: option.optionId, name: typeof option.name === 'string' ? option.name : option.optionId, kind: option.kind }] : [];
    });
    const first = (kind: string) => options.find((option) => option.kind === kind);
    const cancelled = { outcome: { outcome: 'cancelled' } };
    const select = (option: Option | undefined) => option ? { outcome: { outcome: 'selected', optionId: option.optionId } } : cancelled;
    if (!this.turn || this.turn.cancelled || this.closing) { this.reply(id, cancelled); return; }
    if (namesAlondra(toolCall) && first('allow_once') && (this.mode === 'dontAsk' || (this.mode === 'default' && this.acpMode === 'auto'))) { this.reply(id, select(first('allow_once'))); return; }
    if (this.mode === 'dontAsk') { this.reply(id, select(first('reject_once'))); return; }
    const tool = text(toolCall.toolCallId);
    if (!tool || options.length === 0) { this.reply(id, cancelled); return; }
    if (this.asked.size >= MAX_PENDING) { this.fail('native_decision_capacity'); return; }
    const kind: DecisionKind = first('allow_once') || first('reject_once') ? 'permission' : 'question';
    const title = text(toolCall.title) ?? text(toolCall.kind) ?? 'tool';
    // A question is asked as the cards ask one: its answer is the name of the option it chose.
    const questions = kind === 'question' ? { questions: [{ question: title, options: options.filter((option) => !NEVER_CHOSEN.has(option.kind)).map((option) => ({ label: option.name })) }] } : {};
    const payload = JSON.parse(JSON.stringify({
      provider: 'antigravity', method: 'session/request_permission', native: params, tool: title,
      input: { ...questions, options, toolCall: params.toolCall },
    })) as Json;
    const epoch = this.config.observe({ type: 'decision_requested', request_id: key, tool_use_id: tool, kind, payload });
    if (typeof epoch !== 'number') { this.fail('native_decision_not_durable'); return; }
    this.asked.set(key, { id, epoch, kind, payload, tool, options, identity });
  }
  private answer(id: RequestId, key: string, identity: string, result: Json): Promise<void> {
    this.answered.set(key, { identity, result });
    if (this.answered.size > MAX_ANSWERED) this.answered.delete(this.answered.keys().next().value!);
    return this.send({ id, result });
  }
  /** An answer the host gives by itself. It is not kept: a repeated request is decided again. */
  private reply(id: RequestId, result: Json): void {
    void this.send({ id, result }).catch(() => this.fail('native_response_uncertain'));
  }
  respond(request: DecisionRequest, decision: Decision): boolean {
    const entry = this.asked.get(request.request_id);
    if (!entry || entry.epoch !== request.epoch || entry.kind !== request.kind || entry.tool !== request.tool_use_id || !isDeepStrictEqual(entry.payload, request.asked)) return false;
    let result: Json;
    if (decision.behavior === 'allow') {
      const chosen = entry.options.find((option) => option.optionId === decision.updated_input.optionId);
      if (!chosen || NEVER_CHOSEN.has(chosen.kind)) return false;
      result = { outcome: { outcome: 'selected', optionId: chosen.optionId } };
    } else if (decision.behavior === 'deny') {
      const reject = entry.options.find((option) => option.kind === 'reject_once');
      result = reject ? { outcome: { outcome: 'selected', optionId: reject.optionId } } : { outcome: { outcome: 'cancelled' } };
    } else return false;
    this.asked.delete(request.request_id);
    void this.answer(entry.id, request.request_id, entry.identity, result).then(() => this.settled(request.request_id, entry, 'answered'), () => this.fail('native_response_uncertain'));
    return true;
  }
  private settled(key: string, entry: Asked, outcome: 'answered' | 'cancelled'): void {
    this.asked.delete(key);
    this.config.observe({ type: 'decision_settled', request_id: key, epoch: entry.epoch, outcome });
  }
  /** Every request agy still waits on is answered `cancelled`. */
  private cancelAsked(): void {
    for (const [key, entry] of [...this.asked]) {
      this.asked.delete(key);
      void this.send({ id: entry.id, result: { outcome: { outcome: 'cancelled' } } }).catch(() => {});
      this.settled(key, entry, 'cancelled');
    }
  }
  private async cancelTurn(): Promise<void> {
    const turn = this.turn;
    if (!turn || this.closing || this.closed) return;
    turn.cancelled = true;
    await this.send({ method: 'session/cancel', params: { sessionId: this.native } });
    this.cancelAsked();
  }
  async interrupt(): Promise<void> { await this.interruptTurn(); }
  /** agy ends the turn by answering its prompt `cancelled`. */
  async interruptTurn(): Promise<void> {
    await this.ready;
    await this.cancelTurn();
  }
  async forceStop(): Promise<void> {
    try {
      await this.ready;
      await this.cancelTurn();
    } catch { /* The process group is ended either way. */ }
    finally { this.close(); await this.finished; }
  }
  async supportedModels(): Promise<{ value: string }[]> {
    await this.ready;
    return optionValues(this.option('model')).map((value) => ({ value }));
  }
  async setModel(model: string): Promise<void> {
    await this.ready;
    await this.setOption('model', model);
    this.model = model;
    await this.applyEffort();
  }
  async setPermissionMode(mode: Mode): Promise<void> {
    await this.ready;
    this.mode = mode;
    await this.applyMode();
  }
  mcpServerStatus(): Promise<never> { return Promise.reject(new Error('Antigravity does not report MCP status')); }
  reconnectMcpServer(_server: string): Promise<void> { return Promise.reject(new Error('Antigravity does not reconnect MCP servers')); }
  mcpAuthenticate(_server: string): Promise<unknown> { return Promise.reject(new Error('Antigravity does not sign in to MCP servers')); }
  mcpClearAuth(_server: string): Promise<unknown> { return Promise.reject(new Error('Antigravity does not clear MCP authentication')); }
  close(): void {
    clearTimeout(this.openingDeadline);
    if (this.closing || this.closed) return;
    this.closing = true;
    const child = this.child;
    if (!child) {
      queueMicrotask(() => this.end(null, null));
      return;
    }
    let terminated = false;
    let grace: ReturnType<typeof setTimeout> | undefined;
    const terminate = () => {
      if (terminated) return;
      terminated = true;
      clearTimeout(grace);
      child.stdin.destroy();
      this.signal('SIGTERM');
      this.stopDeadline = setTimeout(() => { if (!this.closed) this.signal('SIGKILL'); }, STOP_ESCALATION_MS);
      this.stopDeadline.unref();
    };
    if (this.opened && this.closeAdvertised && !this.failure) {
      grace = setTimeout(terminate, CLOSE_WAIT_MS);
      grace.unref();
      void this.rpc('session/close', { sessionId: this.native }).catch(() => {}).then(terminate);
    } else terminate();
  }
}
