import { createHash } from 'node:crypto';
import { spawn, type ChildProcessWithoutNullStreams } from 'node:child_process';
import { closeSync, constants, fstatSync, fsyncSync, lstatSync, mkdirSync, openSync, readFileSync, renameSync, writeFileSync } from 'node:fs';
import { dirname, isAbsolute, join } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import type { Decision, DecisionKind } from './frame.js';
import { InputNotOffered, type DecisionRequest, type HostConfiguration, type HostSession, type Observation } from './host-session.js';

export const CODEX_VERSION = 'codex-app-server-0.163.0';
export const CODEX_CAPABILITIES = ['streaming_input', 'exact_decisions', 'owner_attach', 'commit_ack', 'interrupt', 'force_stop', 'set_model', 'set_permission_mode', 'mcp_status', 'mcp_sign_in', 'native_account_policy', 'causal_child_continuations'] as const;
const MAX_NATIVE_LINE = 4 * 1024 * 1024;
const MAX_PENDING = 64;
type Json = Record<string, unknown>;
type RequestId = string | number;
type Pending = { method: string; resolve: (value: Json) => void; reject: (error: Error) => void };
type Asked = { id: RequestId; method: string; kind: DecisionKind; epoch: number; payload: Json; tool: string | null };
const object = (value: unknown): Json => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Json : {};
const text = (value: unknown): string | undefined => typeof value === 'string' && value.length > 0 ? value : undefined;
const requestKey = (id: RequestId): string => `${typeof id}:${id}`;

const ROLE_ENVIRONMENT = ['ALONDRA_HOME', 'ALONDRA_DOMAIN_CAPABILITY', 'ALONDRA_DOMAIN_PROTOCOL', 'ALONDRA_SESSION_ID', 'ALONDRA_ORCHESTRATOR', 'ALONDRA_ORCHESTRATOR_EXECUTION', 'ALONDRA_PORT', 'ALONDRA_SANDBOX', 'ALONDRA_BIN', 'ALONDRA_PROVIDER'] as const;
export function alondraMcpEnvironment(config: HostConfiguration): Record<string, string> {
  if (!config.worker && !config.orchestrator) return {};
  const env = Object.fromEntries(ROLE_ENVIRONMENT.flatMap((key) => typeof config.env[key] === 'string' ? [[key, config.env[key] as string]] : []));
  for (const key of ['ALONDRA_HOME', 'ALONDRA_DOMAIN_CAPABILITY', 'ALONDRA_DOMAIN_PROTOCOL', 'ALONDRA_SESSION_ID', 'ALONDRA_PROVIDER']) if (!env[key]) throw new Error(`Alondra role environment is missing ${key}`);
  if (!isAbsolute(env.ALONDRA_HOME ?? '') || env.ALONDRA_DOMAIN_PROTOCOL !== '2' || env.ALONDRA_PROVIDER !== 'codex') throw new Error('Alondra role environment is invalid');
  if (env.ALONDRA_PORT && (!/^\d+$/.test(env.ALONDRA_PORT) || Number(env.ALONDRA_PORT) < 1 || Number(env.ALONDRA_PORT) > 65535)) throw new Error('Alondra role port is invalid');
  if (config.orchestrator && (env.ALONDRA_ORCHESTRATOR !== '1' || !env.ALONDRA_ORCHESTRATOR_EXECUTION)) throw new Error('Alondra orchestrator environment is missing its execution identity');
  if (config.worker && env.ALONDRA_ORCHESTRATOR === '1') throw new Error('An Alondra node cannot inherit orchestrator identity');
  if (config.stateDirectory) {
    if (!isAbsolute(config.stateDirectory) || !config.sessionId || !/^[0-9a-f-]{36}$/.test(config.sessionId)) throw new Error('Alondra root thread mapping identity is invalid');
    env.ALONDRA_CODEX_THREAD_MAPPING = join(dirname(config.stateDirectory), 'native-threads', config.sessionId);
  }
  return env;
}

/** Configuration is compiled for each turn. Developer instructions remain thread scoped. */
export function codexPolicy(config: HostConfiguration, mode: 'default' | 'plan' | 'dontAsk', model?: string): Json {
  const orchestrator = config.orchestrator !== undefined;
  const settings = object(config.worker?.settings.codex ?? config.orchestrator?.settings.codex);
  const effort = text(settings.reasoning_effort) ?? config.orchestrator?.reasoning_effort ?? config.worker?.effort;
  const chosen = model ?? config.worker?.model ?? config.orchestrator?.model;
  return {
    cwd: config.cwd,
    ...(chosen ? { model: chosen } : {}),
    ...(effort ? { effort } : {}),
    approvalPolicy: orchestrator || mode === 'dontAsk' ? 'never' : 'on-request',
    approvalsReviewer: orchestrator || mode === 'dontAsk' ? 'user' : 'auto_review',
    sandboxPolicy: orchestrator || mode === 'plan' ? { type: 'readOnly' } : { type: 'workspaceWrite', writableRoots: [config.cwd], networkAccess: false, excludeTmpdirEnvVar: true, excludeSlashTmp: true },
    ...(orchestrator ? { environments: [] } : {}),
    ...(mode === 'plan' && chosen ? { collaborationMode: { mode: 'plan', settings: { model: chosen, reasoning_effort: effort ?? null, developer_instructions: null } } } :
      chosen ? { collaborationMode: { mode: 'default', settings: { model: chosen, reasoning_effort: effort ?? null, developer_instructions: null } } } : {}),
  };
}

class NativeAccountRefusal extends InputNotOffered {}
type NativeAuthIdentity = { authMethod: 'chatgpt' | 'apikey'; email: string | null; workspaceId: string | null };
/** Owns stdio for its entire lifetime; losing a daemon socket does not close this session. */
export class CodexSession implements HostSession {
  readonly finished: Promise<void>;
  private finish!: () => void;
  private child: ChildProcessWithoutNullStreams;
  private pending = new Map<string, Pending>();
  private asked = new Map<string, Asked>();
  private answered = new Map<string, { identity: string; result: Json }>();
  private sequence = 0;
  private buffer = Buffer.alloc(0);
  private inputs = new Set<string>();
  private queue: { id: string; prompt: string; plan?: { key: string; entry: Asked } }[] = [];
  private answeringPlans = new Set<string>();
  private ready: Promise<void>;
  private authIdentity?: NativeAuthIdentity;
  private workspaceWitness?: string;
  private accountPolicy?: Json;
  private thread?: string;
  private turn?: string;
  private starting = false;
  private notifications: Json[] = [];
  private notificationBytes = 0;
  private closed = false;
  private closing = false;
  /** Why the native session ended without being asked to. */
  failure?: string;
  private pumping = false;
  private mode: 'default' | 'plan' | 'dontAsk' = 'default';
  private model?: string;
  private children = new Set<string>();
  private childStates = new Map<string, { parent: string; activation: string; status: string; pending: boolean; endedStatus?: string; report?: string }>();
  private childChecks = new Set<string>();
  private childItems = new Set<string>();
  private childPolls = new Map<string, ReturnType<typeof setTimeout>>();
  private mapping?: string;

  constructor(private config: HostConfiguration) {
    if (!config.sessionId || !/^[0-9a-f-]{36}$/.test(config.sessionId)) throw new Error('Codex conversation identity absent');
    alondraMcpEnvironment(config);
    this.finished = new Promise((resolve) => { this.finish = resolve; });
    this.model = config.worker?.model ?? config.orchestrator?.model;
    if (object(config.worker?.settings.codex).initial_mode === 'plan') this.mode = 'plan';
    this.config.beforeSpawn?.();
    const accountArgs = config.accountId && config.accountId !== 'default' ? ['-c', 'cli_auth_credentials_store="file"'] : [];
    this.child = spawn(config.executable, [...accountArgs, 'app-server', '--listen', 'stdio://'], {
      cwd: config.cwd, env: config.env, stdio: ['pipe', 'pipe', 'pipe'], windowsHide: true,
    });
    this.child.stdout.on('data', (bytes: Buffer) => this.consume(bytes));
    // Native stderr must always be drained. It is not semantic evidence and may contain credentials.
    this.child.stderr.on('data', () => {});
    this.child.on('error', () => this.fail('native_spawn_failed'));
    this.child.on('exit', (code, signal) => {
      if (!this.closing) this.failure ??= 'native_process_exited';
      this.closed = true;
      for (const pending of this.pending.values()) pending.reject(new Error('Codex process ended'));
      this.pending.clear();
      for (const [key, asked] of this.asked) this.settled(key, asked, 'cancelled');
      this.config.observe({ type: 'process_exited', exit_code: code, signal });
      this.finish();
    });
    this.child.stdin.on('error', () => this.fail('native_input_closed'));
    this.ready = this.initialize();
    void this.ready.catch((error: unknown) => { if (error instanceof NativeAccountRefusal) {if(this.queue.length===0)this.emit('alondra/accountBlocked', { code: error.code }, 'diagnostic');} else this.fail('native_initialization_failed'); });
  }

  private emit(method: string, params: Json, kind = 'message'): number | void {
    return this.config.observe({ type: 'sdk', kind, native: { ...(this.turn ? { turn_id: this.turn } : {}) }, payload: { method, params }, failures: [] });
  }
  private raw(message: Json): void {
    this.config.observe({ type: 'sdk', kind: 'message', native: {}, payload: message, failures: [] });
  }
  private fail(code: string): void {
    if (this.closed || this.closing) return;
    this.failure = code;
    this.emit('alondra/transportFailed', { code }, 'diagnostic');
    this.close();
  }
  private send(message: Json): Promise<void> {
    if (this.closed || this.child.stdin.destroyed) return Promise.reject(new Error('Codex pipe closed'));
    const bytes = JSON.stringify(message) + '\n';
    if (Buffer.byteLength(bytes) > MAX_NATIVE_LINE) return Promise.reject(new Error('Codex request too large'));
    return new Promise((resolve, reject) => this.child.stdin.write(bytes, (error) => error ? reject(error) : resolve()));
  }
  private rpc(method: string, params: Json): Promise<Json> {
    if (this.pending.size >= MAX_PENDING) return Promise.reject(new Error('Codex request capacity'));
    const id = `alondra:${++this.sequence}`;
    return new Promise((resolve, reject) => {
      this.pending.set(requestKey(id), { method, resolve, reject });
      void this.send({ id, method, params }).catch((error: Error) => {
        this.pending.delete(requestKey(id)); reject(error);
      });
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
        const message = object(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(line)));
        if (typeof message.method === 'string') {
          if (typeof message.id === 'string' || typeof message.id === 'number') this.request(message);
          else if (this.starting) {
            this.notificationBytes += line.length;
            if (this.notifications.length >= 4096 || this.notificationBytes > 16 * MAX_NATIVE_LINE) throw new Error('Native notification capacity');
            this.notifications.push(message);
          } else this.notification(message);
        } else if (typeof message.id === 'string' || typeof message.id === 'number') {
          const key = requestKey(message.id); const pending = this.pending.get(key);
          if (!pending) continue;
          this.pending.delete(key);
          if (message.error !== undefined) {
            const error = object(message.error);
            this.config.nativeRpcError?.(pending.method, String(error.code ?? 'failed'), typeof error.message === 'string' ? error.message.slice(0, 2000) : '');
            pending.reject(new Error(`Codex RPC ${String(error.code ?? 'failed')}`));
          } else pending.resolve(object(message.result));
        } else throw new Error('Invalid native message');
      }
    } catch { this.fail('native_protocol_failed'); }
  }
  private privateJson(path: string): Json | undefined {
    let fd: number;
    try { fd = openSync(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK); }
    catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined; throw new NativeAccountRefusal('native_auth_policy_invalid'); }
    try {
      const meta = fstatSync(fd);
      if (!meta.isFile() || meta.nlink !== 1 || meta.uid !== process.getuid?.() || (meta.mode & 0o777) !== 0o600 || meta.size > 4096) throw new NativeAccountRefusal('native_auth_policy_invalid');
      return object(JSON.parse(readFileSync(fd, 'utf8')));
    } catch (error) { if (error instanceof NativeAccountRefusal) throw error; throw new NativeAccountRefusal('native_auth_policy_invalid'); }
    finally { closeSync(fd); }
  }
  private policy(): Json {
    if (this.accountPolicy) return this.accountPolicy;
    const settings = object(this.config.worker?.settings.codex ?? this.config.orchestrator?.settings.codex);
    const policy = { ...object(settings.nativeAccountPolicy) };
    if (this.config.worker && this.config.stateDirectory) {
      const original = this.privateJson(join(this.config.stateDirectory, 'native-account.json'));
      if (typeof original?.email === 'string' && original.email.trim()) policy.expectedEmail = original.email.trim();
      if (typeof original?.native_identity === 'string' && original.native_identity) policy.expectedWorkspaceId = original.native_identity;
    }
    this.accountPolicy = policy;
    return policy;
  }
  private sealAccount(identity: NativeAuthIdentity): void {
    if (identity.workspaceId) {
      if (this.workspaceWitness && this.workspaceWitness !== identity.workspaceId) throw new NativeAccountRefusal('native_account_identity_changed');
      this.workspaceWitness = identity.workspaceId;
    }
    const matches = (original: NativeAuthIdentity | Json) => original.authMethod === identity.authMethod && original.email === identity.email
      && (!original.workspaceId || !identity.workspaceId || original.workspaceId === identity.workspaceId);
    if (this.authIdentity) {
      if (!matches(this.authIdentity)) throw new NativeAccountRefusal('native_account_identity_changed');
      return;
    }
    let original = identity;
    if (this.config.stateDirectory) {
      const file = join(this.config.stateDirectory, 'native-auth-account.json');
      const prior = this.privateJson(file);
      if (prior) {
        if (!matches(prior)) throw new NativeAccountRefusal('native_account_identity_changed');
        if (prior.workspaceId !== null && (typeof prior.workspaceId !== 'string' || !prior.workspaceId || prior.workspaceId.length > 1024)) throw new NativeAccountRefusal('native_auth_policy_invalid');
        if (identity.authMethod === 'apikey') throw new NativeAccountRefusal('native_api_key_resume_unverifiable');
        original = { authMethod: identity.authMethod, email: identity.email, workspaceId: text(prior.workspaceId) ?? null };
        this.workspaceWitness ??= original.workspaceId ?? undefined;
      } else {
        if (this.config.resume) throw new NativeAccountRefusal('native_account_identity_unavailable');
        const parent = lstatSync(this.config.stateDirectory);
        if (!parent.isDirectory() || parent.isSymbolicLink() || parent.uid !== process.getuid?.() || (parent.mode & 0o777) !== 0o700) throw new NativeAccountRefusal('native_auth_policy_invalid');
        const fd = openSync(file, constants.O_WRONLY | constants.O_CREAT | constants.O_EXCL, 0o600);
        try { writeFileSync(fd, JSON.stringify(identity)); fsyncSync(fd); } finally { closeSync(fd); }
        const directory = openSync(this.config.stateDirectory, constants.O_RDONLY); try { fsyncSync(directory); } finally { closeSync(directory); }
      }
    }
    this.authIdentity = original;
  }
  private async verifyAccount(): Promise<void> {
    try {
      const policy = this.policy();
      const effective=object((await this.rpc('config/read',{cwd:this.config.cwd,includeLayers:false})).config);
      const configured=object(object(effective.model_providers).openai);
      const trusted=object(object(this.config.worker?.settings.codex ?? this.config.orchestrator?.settings.codex).config);
      const overrideProvider=object(trusted['model_providers.openai']);
      if ((text(effective.model_provider) ?? 'openai')!=='openai' || (text(trusted.model_provider) ?? 'openai')!=='openai') throw new NativeAccountRefusal('native_model_provider_unsupported');
      const unsafe=['env_key','experimental_bearer_token','auth','gateway_oauth','aws','http_headers','env_http_headers'];
      if ([configured,overrideProvider].some((provider)=>unsafe.some((key)=>provider[key]!==undefined && provider[key]!==null && !(typeof provider[key]==='object' && Object.keys(object(provider[key])).length===0))) || unsafe.some((key)=>trusted[`model_providers.openai.${key}`]!==undefined)) throw new NativeAccountRefusal('native_provider_auth_unsupported');
      if (Object.values(object(effective.agents)).some((role)=>role!==null && typeof role==='object') || Object.keys(trusted).some((key)=>key.startsWith('agents.') && key.endsWith('.config_file'))) throw new NativeAccountRefusal('native_provider_auth_unsupported');
      // This supported metadata RPC distinguishes managed OAuth from external modes; never request a token.
      const status = await this.rpc('getAuthStatus', { includeToken: false, refreshToken: false });
      const read = await this.rpc('account/read', { refreshToken: false });
      if (status.requiresOpenaiAuth !== true || read.requiresOpenaiAuth !== true) throw new NativeAccountRefusal('native_provider_auth_unsupported');
      const mode = text(status.authMethod); const account = object(read.account);
      if (!['chatgpt', 'apikey'].includes(mode ?? '')) throw new NativeAccountRefusal('native_auth_mode_unsupported');
      const identity: NativeAuthIdentity = { authMethod: mode as 'chatgpt' | 'apikey', email: null, workspaceId: null };
      if (mode === 'apikey') {
        if (account.type !== 'apiKey' || policy.paidAccountId !== (this.config.accountId ?? 'default')) throw new NativeAccountRefusal('paid_authorization_required');
      } else {
        if (account.type !== 'chatgpt') throw new NativeAccountRefusal('native_auth_mode_unsupported');
        const email = text(account.email)?.trim().toLowerCase();
        if (!email) throw new NativeAccountRefusal('native_account_identity_unavailable');
        if (email.length > 1024) throw new NativeAccountRefusal('native_account_identity_invalid');
        const usage = await this.rpc('account/rateLimits/read', {}).catch(() => ({} as Json));
        const workspace = typeof usage.ordinaryUsageAllowed === 'boolean' ? text(usage.accountId) : undefined;
        if (workspace && (workspace.length > 1024 || workspace.trim() !== workspace || /[\x00-\x1f\x7f]/.test(workspace))) throw new NativeAccountRefusal('native_account_identity_invalid');
        identity.email = email; identity.workspaceId = workspace ?? null;
        const expected = text(policy.expectedEmail)?.trim().toLowerCase();
        if (expected && expected !== identity.email) throw new NativeAccountRefusal('native_account_identity_changed');
        const expectedWorkspace = text(policy.expectedWorkspaceId);
        if (expectedWorkspace && identity.workspaceId && expectedWorkspace !== identity.workspaceId) throw new NativeAccountRefusal('native_account_identity_changed');
      }
      this.sealAccount(identity);
    } catch (error) { if (error instanceof NativeAccountRefusal) throw error; throw new NativeAccountRefusal('native_auth_unavailable'); }
  }
  private async initialize(): Promise<void> {
    await this.rpc('initialize', { clientInfo: { name: 'alondra', title: 'Alondra', version: this.config.hostRelease ?? 'development' }, capabilities: { experimentalApi: true } });
    await this.send({ method: 'initialized' });
    await this.verifyAccount();
    const settings = object(this.config.worker?.settings.codex ?? this.config.orchestrator?.settings.codex);
    const overrides: Json = { ...object(settings.config), 'features.goals': false };
    if (this.config.orchestrator) {
      // Disabling named inherited servers is required; merely adding our server leaves other tools enabled.
      const read = await this.rpc('config/read', { cwd: this.config.cwd, includeLayers: false });
      const inherited = Object.keys(object(object(read.config).mcp_servers));
      if (inherited.length > 256 || inherited.some((name) => name.length > 256 || !/^[A-Za-z0-9_-]+$/.test(name))) throw new Error('Inherited MCP server names cannot be safely overridden');
      for (const name of inherited) overrides[`mcp_servers.${name}.enabled`] = false;
      this.emit('alondra/orchestratorPolicy', { disabledMcpServers: inherited });
      Object.assign(overrides, {
        'features.shell_tool': false, 'features.stable_environment_tools': false, 'agents.enabled': false,
        'features.multi_agent': false, 'features.goals': false, 'web_search': 'disabled', 'features.apps': false,
        'tools.experimental_request_user_input.enabled': false, 'tools.update_plan.enabled': false,
        'features.plugins': false, 'features.hooks': false, 'features.skill_mcp_dependency_install': false,
        'features.image_generation': false, 'features.imagegenext': false,
        'features.code_mode': false, 'features.code_mode_only': false, 'features.code_mode_host': false,
        'features.code_mode_tool_search': false, 'features.code_mode_only_strict_3p_tools': false,
        'features.code_mode_host_grpc': false, 'features.code_mode_prewarm': false,
        'features.sleep_tool': false, 'features.multi_agent_v2': { enabled: false, disable_native_tools: true },
      });
    }
    const mcp: Json = { ...object(settings.mcp_servers) };
    for (const [name, server] of Object.entries(this.config.orchestrator?.mcp_servers ?? {})) {
      mcp[name] = { command: server.command, args: server.args, required: true, tool_timeout_sec: 3900 };
    }
    const roleEnvironment = alondraMcpEnvironment(this.config);
    if ((this.config.worker || this.config.orchestrator) && !Object.keys(mcp).some((name) => name === 'alondra' || name === 'alondra-orchestrator')) throw new Error('The required Alondra role MCP server is missing');
    for (const [name, server] of Object.entries(mcp)) {
      if (name === 'alondra' || name === 'alondra-orchestrator') {
        const explicit = Object.fromEntries(Object.entries(object(object(server).env)).filter(([key]) => !key.startsWith('ALONDRA_')));
        mcp[name] = { ...object(server), env: { ...explicit, ...roleEnvironment }, env_vars: [], default_tools_approval_mode: 'approve' };
      }
      overrides[`mcp_servers.${name}`] = { ...object(mcp[name]), enabled: true, required: true };
    }
    const args: Json = {
      cwd: this.config.cwd, ...(this.model ? { model: this.model } : {}), config: overrides,
      approvalPolicy: this.config.orchestrator ? 'never' : 'on-request',
      approvalsReviewer: this.config.orchestrator ? 'user' : 'auto_review',
      sandbox: this.config.orchestrator ? 'read-only' : 'workspace-write',
      ...(text(settings.developerInstructions) ? { developerInstructions: settings.developerInstructions } : {}),
    };
    const existing = this.readMapping();
    if (this.config.resume && !existing) throw new Error('Codex native identity unavailable');
    const result = existing ? await this.rpc('thread/resume', { ...args, threadId: existing, excludeTurns: true }) :
      await this.rpc('thread/start', { ...args, ephemeral: false, environments: this.config.orchestrator ? [] : null });
    if (result.modelProvider !== 'openai') throw new NativeAccountRefusal('native_model_provider_unsupported');
    const id = text(object(result.thread).id);
    if (!id || (existing && existing !== id)) throw new Error('Codex thread identity differs');
    this.thread = id;
    this.model ??= text(result.model);
    this.writeMapping(id);
    this.emit('alondra/threadReady', { threadId: id, logicalConversationId: this.config.sessionId, cwd: this.config.cwd, ...(this.model ? { model: this.model } : {}), processGeneration: this.config.processGeneration ?? this.config.sessionId }, 'initialized');
    void this.pump();
  }
  private readMapping(): string | undefined {
    if (!this.config.stateDirectory) return undefined;
    const directory = join(dirname(this.config.stateDirectory), 'native-threads');
    mkdirSync(directory, { recursive: true, mode: 0o700 });
    const meta = lstatSync(directory);
    if (!meta.isDirectory() || meta.isSymbolicLink() || meta.uid !== process.getuid?.() || (meta.mode & 0o777) !== 0o700) throw new Error('Private thread directory refused');
    this.mapping = join(directory, this.config.sessionId!);
    try {
      const node = lstatSync(this.mapping);
      if (!node.isFile() || node.isSymbolicLink() || node.nlink !== 1 || node.uid !== process.getuid?.() || (node.mode & 0o777) !== 0o600 || node.size > 1024) throw new Error('Private native thread refused');
      const retained = object(JSON.parse(readFileSync(this.mapping, 'utf8')));
      const home = createHash('sha256').update(this.config.env.CODEX_HOME ?? 'native_default').digest('hex');
      // Instructions change with each release; the same thread resumes under the current ones.
      if (retained.accountId !== (this.config.accountId ?? 'default') || retained.homeDigest !== home) throw new Error('Native thread credential home differs');
      const id = text(retained.threadId);
      if (!id || id.length > 256) throw new Error('Native thread identity refused');
      return id;
    } catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return undefined; throw error; }
  }
  private instructionsDigest(): string {
    return createHash('sha256').update(String(object(this.config.worker?.settings.codex ?? this.config.orchestrator?.settings.codex).developerInstructions ?? '')).digest('hex');
  }
  private writeMapping(threadId: string): void {
    if (!this.mapping) return;
    const next = this.mapping + '.next';
    const fd = openSync(next, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
    try { writeFileSync(fd, JSON.stringify({ threadId, accountId: this.config.accountId ?? 'default', homeDigest: createHash('sha256').update(this.config.env.CODEX_HOME ?? 'native_default').digest('hex'), instructionsDigest: this.instructionsDigest() })); fsyncSync(fd); } finally { closeSync(fd); }
    renameSync(next, this.mapping);
    const directory = openSync(dirname(this.mapping), constants.O_RDONLY);
    try { fsyncSync(directory); } finally { closeSync(directory); }
  }

  offer(inputId: string, prompt: string): void | Promise<void> {
    if (this.closed || this.closing) throw new InputNotOffered(this.failure ?? 'native_session_ended');
    if (this.inputs.has(inputId) || this.queue.length >= 64) throw new Error('Codex input refused');
    if (Buffer.byteLength(prompt) > 900_000) throw new Error('Codex input capacity');
    this.inputs.add(inputId); this.queue.push({ id: inputId, prompt });
    void this.pump();
    if(this.config.worker && !this.thread)return this.ready.catch((error: unknown) => { throw error instanceof InputNotOffered ? error : new InputNotOffered('native_initialization_failed'); });
  }
  private async pump(): Promise<void> {
    if (this.pumping || this.closed || this.closing || this.turn || this.queue.length === 0) return;
    this.pumping = true;
    let input = this.queue[0];
    try {
      await this.ready;
      if (this.turn || this.closed || this.closing) return;
      await this.verifyAccount();
      if (this.turn || this.closed || this.closing) return;
      input = this.queue.shift(); if (!input) return;
      this.starting = true;
      this.emit('alondra/inputStarting',{threadId:this.thread,inputId:input.id});
      const response = await this.rpc('turn/start', {
        threadId: this.thread, clientUserMessageId: input.id,
        input: [{ type: 'text', text: input.prompt, text_elements: [] }], ...codexPolicy(this.config, this.mode, this.model),
      });
      const turn = text(object(response.turn).id);
      if (!turn) throw new Error('Codex turn identity absent');
      this.turn = turn;
      if (input.plan) {
        this.asked.delete(input.plan.key);
        this.answeringPlans.delete(input.plan.key);
        this.emit('alondra/planContinuationAccepted', { threadId: this.thread, turnId: turn, inputId: input.id, requestId: input.plan.key, epoch: input.plan.entry.epoch });
      } else this.emit('alondra/inputAccepted', { threadId: this.thread, turnId: turn, inputId: input.id, ...(this.model ? { model: this.model } : {}) });
      this.starting = false;
      for (const message of this.notifications.splice(0)) this.notification(message);
      this.notificationBytes = 0;
    } catch (error) {
      if (error instanceof NativeAccountRefusal) {
        if (input && this.queue[0] === input) this.queue.shift();
        if (input?.plan) { this.answeringPlans.delete(input.plan.key); this.settled(input.plan.key, input.plan.entry, 'cancelled'); }
        else if (input) this.emit('alondra/inputRefused', { threadId: this.thread ?? null, inputId: input.id, nativeSubmitted: false, code: error.code }, 'diagnostic');
        this.emit('alondra/accountBlocked', { threadId: this.thread ?? null, code: error.code }, 'diagnostic');
      } else this.fail('native_turn_start_uncertain');
    }
    finally { this.pumping = false; if (!this.turn && this.queue.length > 0 && !this.closed && !this.closing) void this.pump(); }
  }
  private notification(message: Json): void {
    this.raw(message);
    const params = object(message.params);
    const nativeThread = text(params.threadId);
    if (nativeThread && (nativeThread === this.thread || this.children.has(nativeThread)) && (message.method === 'item/started' || message.method === 'item/completed')) this.childActivity(object(params.item), message.method === 'item/started', nativeThread);
    if (nativeThread && this.children.has(nativeThread) && message.method === 'item/completed') void this.checkChild(nativeThread);
    if (message.method === 'item/completed') {
      const item = object(params.item);
      if (item.type === 'plan' && params.threadId === this.thread && !this.config.orchestrator && typeof item.id === 'string' && typeof item.text === 'string') {
        const key = `plan:${this.config.processGeneration ?? this.config.sessionId}:${item.id}`;
        if (!this.asked.has(key) && !this.answered.has(key)) {
          const payload: Json = { provider: 'codex', method: 'alondra/planApproval', native: params, tool: 'plan', input: { plan: item.text } };
          const epoch = this.config.observe({ type: 'decision_requested', request_id: key, tool_use_id: item.id, kind: 'plan', payload });
          if (typeof epoch !== 'number') { this.fail('native_plan_not_durable'); return; }
          this.asked.set(key, { id: key, method: 'alondra/planApproval', kind: 'plan', epoch, payload, tool: item.id });
        }
      }
    }
    if (message.method === 'turn/started' && params.threadId === this.thread && text(object(params.turn).id)) this.turn = text(object(params.turn).id);
    if (message.method === 'turn/completed' && params.threadId === this.thread && object(params.turn).id === this.turn) {
      this.turn = undefined;
      if (this.config.orchestrator) this.close();
      else void this.pump();
    }
  }
  private childActivity(item: Json, started: boolean, parent: string): void {
    const id = text(item.id); if (!id) return;
    if (!['collabAgentToolCall', 'subAgentActivity'].includes(String(item.type))) return;
    const key = `${parent}:${started ? 'started' : 'completed'}:${id}`;
    if (this.childItems.has(key)) return;
    if (this.childItems.size >= 8192) { this.fail('managed_child_event_capacity'); return; }
    this.childItems.add(key);
    if (item.type === 'collabAgentToolCall') {
      const activating = ['spawnAgent', 'sendInput', 'resumeAgent', 'followupTask'].includes(String(item.tool));
      const states = object(item.agentsStates);
      for (const child of Array.isArray(item.receiverThreadIds) ? item.receiverThreadIds : []) {
        if (typeof child !== 'string' || !child || child === this.thread || child === parent) continue;
        if (!this.children.has(child) && !activating) continue;
        if (this.children.size >= 64 && !this.children.has(child)) { this.fail('managed_child_capacity'); return; }
        this.children.add(child);
        const old = this.childStates.get(child);
        const state = object(states[child]);
        const status = text(state.status) ?? (started ? 'running' : old?.status ?? 'unknown');
        this.childStates.set(child, { parent, activation: activating ? id : old?.activation ?? id, status, pending: activating && started, ...(text(state.message) ? { report: text(state.message) } : old?.report && (!activating || old.activation === id) ? { report: old.report } : {}) });
        if (!started) void this.checkChild(child);
      }
    } else if (item.type === 'subAgentActivity') {
      const child = text(item.agentThreadId); const old = child ? this.childStates.get(child) : undefined;
      if (!child || !old) return;
      const active = item.kind === 'started' || item.kind === 'interacted';
      const status = item.kind === 'completed' ? 'completed' : item.kind === 'interrupted' ? 'interrupted' : old.status;
      this.childStates.set(child, { ...old, activation: active ? id : old.activation, status: active ? 'running' : status, pending: false, ...(active ? { report: undefined } : {}) });
      if (!active) void this.checkChild(child);
    }
  }
  private async backgroundTerminals(threadId: string): Promise<Json[]> {
    const terminals: Json[] = []; const cursors = new Set<string>(); let cursor: string | null = null;
    do {
      const page = await this.rpc('thread/backgroundTerminals/list', { threadId, cursor, limit: 100 });
      if (!Array.isArray(page.data)) throw new Error('Native background inventory absent');
      for (const terminal of page.data) { const value = object(terminal); if (!text(value.processId)) throw new Error('Native terminal identity absent'); terminals.push(value); }
      if (terminals.length > 6400) throw new Error('Native background inventory capacity');
      const next = page.nextCursor;
      if (next !== null && next !== undefined && typeof next !== 'string') throw new Error('Native background cursor invalid');
      cursor = typeof next === 'string' ? next : null;
      if (cursor && cursors.has(cursor)) throw new Error('Native background cursor repeated');
      if (cursor) cursors.add(cursor);
    } while (cursor);
    return terminals;
  }
  private scheduleChild(child: string): void {
    if (!this.childPolls.has(child) && !this.closed && !this.closing) this.childPolls.set(child, setTimeout(() => { this.childPolls.delete(child); void this.checkChild(child); }, 250));
  }
  private async childHistory(child: string): Promise<{ report?: string; active: boolean }> {
    const latest = await this.rpc('thread/turns/list', { threadId: child, limit: 1, sortDirection: 'desc', itemsView: 'notLoaded' });
    if (!Array.isArray(latest.data)) throw new Error('Native child turn inventory absent');
    const turn = object(latest.data[0]);
    if (latest.data.length && !['completed', 'interrupted', 'failed'].includes(String(turn.status))) return { active: true };
    const items: Json[] = []; const cursors = new Set<string>(); let cursor: string | null = null;
    do {
      const page = await this.rpc('thread/items/list', { threadId: child, cursor, limit: 16, sortDirection: 'desc' });
      if (!Array.isArray(page.data)) throw new Error('Native child history absent');
      for (const entry of page.data) items.push({ ...object(object(entry).item), native_turn_id: object(entry).turnId });
      if (items.length > 2048) throw new Error('Native child history observation capacity');
      if (page.nextCursor !== null && page.nextCursor !== undefined && typeof page.nextCursor !== 'string') throw new Error('Native child history cursor invalid');
      cursor = typeof page.nextCursor === 'string' ? page.nextCursor : null;
      if (cursor && cursors.has(cursor)) throw new Error('Native child history cursor repeated');
      if (cursor) cursors.add(cursor);
    } while (cursor);
    const report = items.find((item) => item.native_turn_id === turn.id && item.type === 'agentMessage' && (item.phase === 'final_answer' || item.phase === null) && typeof item.text === 'string');
    for (const item of items.reverse()) this.childActivity(item, false, child);
    return { active: false, ...(typeof report?.text === 'string' ? { report: report.text } : {}) };
  }
  private async checkChild(child: string): Promise<void> {
    const state = this.childStates.get(child);
    if (!state || state.pending || !['completed', 'errored', 'interrupted', 'shutdown', 'quiescent'].includes(state.status) || this.childChecks.has(child) || this.closed || this.closing) return;
    this.childChecks.add(child);
    try {
      const history = await this.childHistory(child);
      if (history.active) { this.scheduleChild(child); return; }
      const report = history.report ?? state.report;
      const terminals = await this.backgroundTerminals(child);
      const current = this.childStates.get(child);
      if (!current || current.activation !== state.activation || current.status !== state.status || current.pending || this.closed || this.closing) return;
      if (terminals.length) { this.scheduleChild(child); return; }
      this.childStates.set(child, { ...state, status: 'quiescent', endedStatus: state.endedStatus ?? state.status, ...(report ? { report } : {}) });
      if ([...this.childStates.values()].some((other) => !['quiescent', 'settled'].includes(other.status))) { this.scheduleChild(child); return; }
      for (const [id, completed] of this.childStates) {
        if (completed.status !== 'quiescent') continue;
        this.emit('alondra/childQuiescent', { threadId: this.thread, childThreadId: id, activationKey: completed.activation, status: completed.endedStatus ?? 'unknown', ...(completed.report ? { report: completed.report } : {}) });
        this.childStates.set(id, { ...completed, status: 'settled' });
      }
    } catch {
      if (!this.closed && !this.closing) this.emit('alondra/childObservationUncertain', { threadId: this.thread, childThreadId: child }, 'diagnostic');
    } finally {
      this.childChecks.delete(child);
      const current = this.childStates.get(child);
      if (current && current.status !== 'settled' && (current.activation !== state.activation || current.status !== state.status)) this.scheduleChild(child);
    }
  }
  private request(message: Json): void {
    const id = message.id as RequestId;
    const method = String(message.method);
    const params = object(message.params);
    const kind: DecisionKind | undefined = method === 'item/tool/requestUserInput' ? 'question' :
      method === 'mcpServer/elicitation/request' ? 'elicitation' :
      ['item/commandExecution/requestApproval', 'item/fileChange/requestApproval', 'item/permissions/requestApproval'].includes(method) ? 'permission' : undefined;
    if (!kind || this.config.orchestrator) {
      void this.send({ id, error: { code: -32601, message: 'This Alondra host does not expose that request' } }).catch(() => this.fail('native_response_failed'));
      return;
    }
    const key = requestKey(id);
    const identity = createHash('sha256').update(JSON.stringify({ method, params })).digest('hex');
    const answered = this.answered.get(key);
    if (answered) {
      if (answered.identity !== identity) { this.fail('native_answered_request_identity_conflict'); return; }
      void this.send({ id, result: answered.result }).catch(() => this.fail('native_response_failed'));
      return;
    }
    const previous = this.asked.get(key);
    if (previous) {
      if (previous.method !== method || !isDeepStrictEqual(previous.payload.native, params)) this.fail('native_request_identity_conflict');
      return;
    }
    if (this.answered.size >= 256) { this.fail('native_decision_history_capacity'); return; }
    if (this.asked.size >= MAX_PENDING) { this.fail('native_decision_capacity'); return; }
    const tool = text(params.itemId) ?? null;
    const displayTool = method === 'item/commandExecution/requestApproval' ? 'exec_command' : method === 'item/fileChange/requestApproval' ? 'apply_patch' : method === 'item/permissions/requestApproval' ? 'request_permissions' : 'request_user_input';
    const payload: Json = {
      provider: 'codex', method, native: params, tool: displayTool,
      input: kind === 'question' ? { questions: params.questions } : { ...params, ...(params.reason !== undefined ? { description: params.reason } : {}) },
      ...(kind === 'elicitation' ? Object.fromEntries(Object.entries({ serverName: params.serverName, message: params.message ?? params.description, requestedSchema: params.requestedSchema, url: params.url }).filter(([, value]) => value !== undefined)) : {}),
    };
    const epoch = this.config.observe({ type: 'decision_requested', request_id: key, ...(tool ? { tool_use_id: tool } : {}), kind, payload });
    if (typeof epoch !== 'number') { this.fail('native_decision_not_durable'); return; }
    this.asked.set(key, { id, method, kind, epoch, payload, tool });
  }
  respond(request: DecisionRequest, decision: Decision): boolean {
    const entry = this.asked.get(request.request_id);
    if (!entry || entry.epoch !== request.epoch || entry.kind !== request.kind || entry.tool !== request.tool_use_id || !isDeepStrictEqual(entry.payload, request.asked)) return false;
    if (entry.method === 'alondra/planApproval') {
      if (this.answeringPlans.has(request.request_id) || !['allow', 'deny'].includes(decision.behavior) || this.queue.length >= 64) return false;
      const allow = decision.behavior === 'allow';
      this.mode = allow ? 'default' : 'plan';
      this.answeringPlans.add(request.request_id);
      this.answered.set(request.request_id, { identity: createHash('sha256').update(JSON.stringify(entry.payload)).digest('hex'), result: { local: true } });
      this.queue.unshift({ id: `plan:${entry.epoch}`, prompt: allow ? 'Implement the plan the user just approved. Continue the Alondra task and report its result through Alondra.' : `Revise the plan using this user feedback: ${decision.behavior === 'deny' ? decision.message : ''}`, plan: { key: request.request_id, entry } });
      void this.pump();
      return true;
    }
    let result: Json;
    const allow = decision.behavior === 'allow';
    if (entry.method === 'item/tool/requestUserInput') {
      if (!allow) result = { answers: {} };
      else {
        const answers = object(decision.updated_input.answers);
        const questions = object(entry.payload.native).questions;
        if (!Array.isArray(questions) || questions.some((q) => !Array.isArray(object(answers[String(object(q).id)]).answers))) return false;
        result = { answers };
      }
    } else if (entry.method === 'item/permissions/requestApproval') {
      result = { permissions: allow ? object(object(entry.payload.native).permissions) : {}, scope: 'turn', strictAutoReview: true };
    } else if (entry.method === 'mcpServer/elicitation/request') {
      result = { action: decision.behavior === 'accept' ? 'accept' : 'decline', content: decision.behavior === 'accept' ? decision.content ?? null : null, _meta: null };
    } else {
      const available = object(entry.payload.native).availableDecisions;
      if (allow && Array.isArray(available) && !available.includes('accept')) return false;
      result = { decision: allow ? 'accept' : 'decline' };
    }
    const identity = createHash('sha256').update(JSON.stringify({ method: entry.method, params: entry.payload.native })).digest('hex');
    this.answered.set(request.request_id, { identity, result });
    this.asked.delete(request.request_id);
    void this.send({ id: entry.id, result }).then(() => this.settled(request.request_id, entry, 'answered'), () => this.fail('native_response_uncertain'));
    return true;
  }
  private settled(key: string, entry: Asked, outcome: 'answered' | 'cancelled'): void {
    this.asked.delete(key);
    this.config.observe({ type: 'decision_settled', request_id: key, epoch: entry.epoch, outcome });
  }
  async interrupt(): Promise<void> { await this.interruptTurn(); }
  async interruptTurn(): Promise<void> {
    await this.ready;
    if (this.turn) await this.rpc('turn/interrupt', { threadId: this.thread, turnId: this.turn });
  }
  async forceStop(): Promise<void> {
    await this.ready;
    try {
      if (this.turn) await this.rpc('turn/interrupt', { threadId: this.thread, turnId: this.turn });
      for (const threadId of [this.thread, ...this.children]) {
        if (!threadId) continue;
        if (threadId !== this.thread) {
          const read = await this.rpc('thread/turns/list', { threadId, limit: 1, sortDirection: 'desc', itemsView: 'notLoaded' });
          const turns = read.data;
          const running = (Array.isArray(turns) ? turns : []).map(object).find((turn) => turn.status === 'inProgress');
          if (running && text(running.id)) await this.rpc('turn/interrupt', { threadId, turnId: running.id });
        }
        for (const terminal of await this.backgroundTerminals(threadId)) await this.rpc('thread/backgroundTerminals/terminate', { threadId, processId: terminal.processId });
      }
    } finally { this.close(); await this.finished; }
  }
  async supportedModels(): Promise<{ value: string }[]> {
    await this.ready;
    const models: { value: string }[] = [];
    let cursor: unknown = null;
    do {
      const page = await this.rpc('model/list', { cursor, limit: 100, includeHidden: false });
      for (const entry of Array.isArray(page.data) ? page.data : []) { const value = text(object(entry).model); if (value) models.push({ value }); }
      cursor = page.nextCursor;
      if (models.length > 1000) throw new Error('Codex model catalog capacity');
    } while (typeof cursor === 'string');
    return models;
  }
  async setModel(model: string): Promise<void> { await this.ready; this.model = model; }
  async setPermissionMode(mode: 'default' | 'plan' | 'dontAsk'): Promise<void> { await this.ready; this.mode = mode; }
  async mcpServerStatus(): Promise<{ name: unknown; status: unknown; error?: unknown }[]> {
    await this.ready;
    const servers: { name: unknown; status: unknown; error?: unknown }[] = [];
    let cursor: unknown = null;
    do {
      const page = await this.rpc('mcpServerStatus/list', { threadId: this.thread, cursor, limit: 100, detail: 'full' });
      for (const server of Array.isArray(page.data) ? page.data : []) {
        const value = object(server);
        servers.push({ name: value.name, status: value.runtimeStatus ?? value.authStatus ?? 'unknown', ...(value.toolsError ? { error: value.toolsError } : {}) });
      }
      cursor = page.nextCursor;
      if (servers.length > 256) throw new Error('Codex MCP status capacity');
    } while (typeof cursor === 'string');
    return servers;
  }
  reconnectMcpServer(_server: string): Promise<void> { return Promise.reject(new Error('Codex MCP reconnect requires a new thread')); }
  async mcpAuthenticate(server: string): Promise<unknown> {
    await this.ready;
    const result = await this.rpc('mcpServer/oauth/login', { threadId: this.thread, name: server });
    return { authUrl: result.authorizationUrl, requiresUserAction: true };
  }
  mcpClearAuth(_server: string): Promise<unknown> { return Promise.reject(new Error('Codex MCP clear authentication is unavailable')); }
  close(): void {
    for (const timer of this.childPolls.values()) clearTimeout(timer); this.childPolls.clear();
    if (this.closing || this.closed) return;
    this.closing = true;
    this.queue = [];
    this.child.stdin.end();
    this.child.kill('SIGTERM');
  }
}
