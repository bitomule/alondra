import { query, type CanUseTool, type ElicitationResult, type HookCallback, type OnElicitation, type Options, type Query, type SDKUserMessage, type PermissionResult, type SpawnOptions, type SpawnedProcess } from '@anthropic-ai/claude-agent-sdk/core';
import { spawn, type ChildProcess } from 'node:child_process';
import { isAbsolute } from 'node:path';
import { isDeepStrictEqual } from 'node:util';
import type { OrchestratorProfile, WorkerProfile } from './bootstrap.js';
import { fitValue, framingProblem, hasToolUse, jsonEvidence, MAX_DECISION_BYTES, MAX_DIAGNOSTIC_BYTES, type Decision, type DecisionKind } from './frame.js';

export type { Failure, Observation, DecisionRequest } from './host-session.js';
import { InputNotOffered, type Failure, type Observation, type DecisionRequest, type HostConfiguration } from './host-session.js';
export type Configuration = HostConfiguration & {
  spawn?: (options: SpawnOptions) => SpawnedProcess & Pick<ChildProcess, 'pid'>;
};

class Input implements AsyncIterable<SDKUserMessage> {
  private items: SDKUserMessage[] = [];
  private wake?: () => void;
  private closed = false;
  offer(message: SDKUserMessage): void {
    if (this.closed) throw new InputNotOffered('native_session_ended');
    if (this.items.length >= 64) throw new Error('Input capacity exceeded');
    if (Buffer.byteLength(JSON.stringify(message)) > 1_048_576) throw new Error('Input is too large');
    this.items.push(message);
    this.wake?.();
  }
  close(): void { this.closed = true; this.wake?.(); }
  async *[Symbol.asyncIterator](): AsyncGenerator<SDKUserMessage> {
    while (true) {
      const item = this.items.shift();
      if (item) { yield item; continue; }
      if (this.closed) return;
      await new Promise<void>((resolve) => { this.wake = resolve; });
      this.wake = undefined;
    }
  }
}

const systemMessages = new Set(['init', 'compact_boundary', 'status', 'api_retry', 'local_command_output', 'hook_started', 'hook_progress', 'hook_response', 'plugin_install', 'task_notification', 'task_started', 'task_updated', 'task_progress', 'background_tasks_changed', 'thinking_tokens', 'session_state_changed', 'worker_shutting_down', 'commands_changed', 'notification', 'files_persisted', 'memory_recall', 'elicitation_complete', 'permission_denied', 'informational', 'conversation_reset', 'mirror_error', 'model_refusal_fallback', 'model_refusal_no_fallback', 'control_request_progress', 'dev_intent', 'session_title_changed']);
const nonFailureTerminalReasons = new Set(['completed', 'background_requested', 'tool_deferred']);
export function classify(payload: Record<string, unknown>): Observation & { type: 'sdk' } {
  const failures: Failure[] = [];
  const failure = (surface: string, code: unknown) => failures.push({ surface, code: String(code), details: payload });
  let kind = 'unsupported';
  if (payload.type === 'result') {
    kind = 'result';
    if (payload.startup_failure_reason !== undefined) failure('startup', payload.startup_failure_reason);
    if (payload.subtype !== 'success' || payload.is_error !== false) failure('result', payload.subtype === 'success' ? 'success_is_error' : payload.subtype);
    if (payload.terminal_reason !== undefined && (payload.is_error !== false || !nonFailureTerminalReasons.has(String(payload.terminal_reason)))) failure('terminal', payload.terminal_reason);
  } else if (payload.type === 'assistant') {
    kind = 'message';
    if (payload.error !== undefined) failure('assistant', payload.error);
  } else if (payload.type === 'auth_status') {
    kind = 'status';
    if (typeof payload.error === 'string' && payload.error.length > 0) failure('auth', 'auth_status_error');
  } else if (payload.type === 'system' && systemMessages.has(String(payload.subtype))) {
    kind = payload.subtype === 'init' ? 'initialized' : String(payload.subtype).startsWith('task_') || payload.subtype === 'background_tasks_changed' ? 'task' : 'status';
    if (payload.subtype === 'hook_response' && payload.outcome !== 'success') failure('hook', payload.outcome);
    if (payload.subtype === 'mirror_error') failure('mirror', 'mirror_error');
    else if (payload.subtype !== 'api_retry' && (payload.error !== undefined || payload.errors !== undefined || (Array.isArray(payload.settings_parse_errors) && payload.settings_parse_errors.length > 0))) failure('settings', 'settings_parse_error');
    if (Array.isArray(payload.plugin_errors) && payload.plugin_errors.length > 0) failure('settings', 'plugin_error');
  } else if (['user', 'stream_event'].includes(String(payload.type))) kind = 'message';
  else if (payload.type === 'command_lifecycle') kind = 'status';
  else if (['tool_progress', 'tool_use_summary', 'rate_limit_event', 'prompt_suggestion', 'mirror_error'].includes(String(payload.type))) {
    kind = 'status';
    if (payload.type === 'rate_limit_event') {
      const info = payload.rate_limit_info as Record<string, unknown> | undefined;
      if (info?.status === 'rejected') failure('rate_limit', 'rejected');
      else if (!['allowed', 'allowed_warning'].includes(String(info?.status))) { kind = 'unsupported'; }
    }
    if (payload.type === 'mirror_error') failure('mirror', 'mirror_error');
  }
  if (kind === 'unsupported') failure('transport', 'unknown_sdk_message');
  const native: Record<string, string> = {};
  for (const [field, source] of Object.entries({ message_id: 'uuid', turn_id: 'turn_id', tool_use_id: 'tool_use_id', parent_tool_use_id: 'parent_tool_use_id', task_id: 'task_id', run_id: 'run_id' })) {
    if (typeof payload[source] === 'string') native[field] = payload[source];
  }
  return { type: 'sdk', kind, native, payload, failures };
}

function safeDiagnostic(text: string): string {
  return text.replace(/sk-ant-[A-Za-z0-9_-]+/g, '[redacted]').replace(/(authorization|x-api-key)[:=]\s*[^\r\n]+/gi, '$1: [redacted]');
}

const MAX_PENDING_DECISIONS = 32;
/** How many tool calls' folders the host remembers while it waits to know whether they ask. */
const MAX_FOLDERS = 64;
/** How long a force stop gives Claude's process to exit after each step before it takes the next. */
const STOP_GRACE_MS = 2_000;
const DECISION_TOO_LARGE = 'The request is too large for Alondra to show; split it into smaller steps';
const defined = (fields: Record<string, unknown>) => Object.fromEntries(Object.entries(fields).filter(([, value]) => value !== undefined));
const decisionKind = (tool: string): DecisionKind => tool === 'AskUserQuestion' ? 'question' : tool === 'ExitPlanMode' ? 'plan' : 'permission';
/** A string is why no answer reached the request: Claude is denied, never granted by default. */
type Outcome = Decision | string;
const permissionResult = (outcome: Outcome): PermissionResult => {
  if (typeof outcome === 'string') return { behavior: 'deny', message: outcome };
  if (outcome.behavior === 'allow') return { behavior: 'allow', updatedInput: outcome.updated_input };
  return { behavior: 'deny', message: outcome.behavior === 'deny' ? outcome.message : 'Permission request cancelled' };
};
/** An approved plan leaves plan mode for auto mode, whatever mode Claude Code remembers from before it. */
const planResult = (outcome: Outcome): PermissionResult => {
  const result = permissionResult(outcome);
  return result.behavior === 'allow' ? { ...result, updatedPermissions: [{ type: 'setMode', mode: 'auto', destination: 'session' }] } : result;
};
const elicitationResult = (outcome: Outcome): ElicitationResult => {
  if (typeof outcome === 'string') return { action: 'cancel' };
  if (outcome.behavior === 'accept') return outcome.content ? { action: 'accept', content: outcome.content as ElicitationResult['content'] } : { action: 'accept' };
  return outcome.behavior === 'decline' ? { action: 'decline' } : { action: 'cancel' };
};
type Pending = { kind: DecisionKind; toolUseId: string | null; asked: unknown; epoch?: number; outcome?: 'answered' | 'cancelled'; settled: boolean; finish: (outcome: Outcome) => void };

export class PermissionCallbacks {
  private pending = new Map<string, Pending>();
  private folders = new Map<string, string>();
  private closed = false;
  constructor(private observe: Configuration['observe']) {}
  /** Claude Code's folder before each command, which follows its `cd`s: a prompt says where its command runs. */
  folder: HookCallback = async (input) => {
    if (input.hook_event_name === 'PreToolUse' && typeof input.cwd === 'string') {
      this.folders.delete(input.tool_use_id);
      this.folders.set(input.tool_use_id, input.cwd);
      for (const oldest of this.folders.keys()) {
        if (this.folders.size <= MAX_FOLDERS) break;
        this.folders.delete(oldest);
      }
    }
    return {};
  };
  request: CanUseTool = (tool, input, options) => {
    const { signal, ...request } = options;
    const kind = decisionKind(tool);
    const cwd = this.folders.get(options.toolUseID);
    this.folders.delete(options.toolUseID);
    return this.ask(options.requestId, kind, options.toolUseID, signal, defined({ tool, input, request: defined(request), cwd }), kind === 'plan' ? planResult : permissionResult);
  };
  elicitation: OnElicitation = (request, { signal, requestId }) => this.ask(requestId, 'elicitation', null, signal, defined({
    serverName: request.serverName, message: request.message, mode: request.mode, url: request.url, elicitationId: request.elicitationId,
    requestedSchema: request.requestedSchema, title: request.title, displayName: request.displayName, description: request.description,
  }), elicitationResult);
  private ask<T>(requestId: string, kind: DecisionKind, toolUseId: string | null, signal: AbortSignal, payload: Record<string, unknown>, answer: (outcome: Outcome) => T): Promise<T> {
    if (this.closed || signal.aborted) return Promise.resolve(answer('Permission request cancelled'));
    if (this.pending.has(requestId)) return Promise.resolve(answer('Duplicate permission request'));
    if (this.pending.size >= MAX_PENDING_DECISIONS) return Promise.resolve(answer('Permission request capacity exceeded'));
    const { bytes, sha256 } = jsonEvidence(payload);
    const problem = bytes === null || bytes > MAX_DECISION_BYTES ? 'size' : framingProblem({ body: { action: { decision: { updated_input: payload } } } });
    if (problem === 'size' || problem === 'depth' || problem === 'lone_surrogate') {
      const details = { request_id: requestId, kind, bytes, sha256, reason: problem };
      try { this.observe({ type: 'sdk', kind: 'diagnostic', native: {}, payload: details, failures: [{ surface: 'transport', code: 'decision_too_large', details }] }); } catch { /* The host records its own storage failure. */ }
      return Promise.resolve(answer(DECISION_TOO_LARGE));
    }
    return new Promise<T>((resolve) => {
      const entry: Pending = { kind, toolUseId, asked: JSON.parse(JSON.stringify(payload)), settled: false, finish: (outcome) => {
        if (this.pending.get(requestId) !== entry) return;
        signal.removeEventListener('abort', cancel);
        this.pending.delete(requestId);
        resolve(answer(outcome));
        entry.outcome = typeof outcome === 'string' ? 'cancelled' : 'answered';
        this.settle(requestId, entry);
      } };
      const cancel = () => entry.finish('Permission request cancelled');
      this.pending.set(requestId, entry);
      signal.addEventListener('abort', cancel, { once: true });
      let epoch: unknown;
      try { epoch = this.observe({ type: 'decision_requested', request_id: requestId, ...(toolUseId === null ? {} : { tool_use_id: toolUseId }), kind, payload }); } catch { epoch = undefined; }
      if (typeof epoch !== 'number' || !Number.isSafeInteger(epoch) || epoch < 1) { entry.finish('Permission delivery failed'); return; }
      entry.epoch = epoch;
      this.settle(requestId, entry);
    });
  }
  private settle(requestId: string, entry: Pending): void {
    if (entry.settled || entry.epoch === undefined || entry.outcome === undefined) return;
    entry.settled = true;
    try { this.observe({ type: 'decision_settled', request_id: requestId, epoch: entry.epoch, outcome: entry.outcome }); } catch { /* The host records its own storage failure. */ }
  }
  /** Applies the decision only to the request pending under this exact identity; false means it reached nothing. */
  respond(request: DecisionRequest, decision: Decision): boolean {
    const entry = this.pending.get(request.request_id);
    if (!entry || entry.epoch === undefined || entry.epoch !== request.epoch || entry.kind !== request.kind || entry.toolUseId !== request.tool_use_id) return false;
    if (!isDeepStrictEqual(entry.asked, request.asked)) return false;
    if (hasToolUse(entry.kind) !== (decision.behavior === 'allow' || decision.behavior === 'deny')) return false;
    entry.finish(decision);
    return true;
  }
  close(): void {
    this.closed = true;
    for (const entry of [...this.pending.values()]) entry.finish('Session ended');
  }
}

/** Laboratory adapter: no persistence, adoption or authorization boundary yet. */
export class Session {
  private input = new Input();
  private controller = new AbortController();
  private query: Query;
  private permissions: PermissionCallbacks;
  private inputs = new Set<string>();
  private seenResult = false;
  private accountReported = false;
  private turnOpen = false;
  private closing = false;
  private child?: SpawnedProcess;
  private childEnded: Promise<void> = Promise.resolve();
  private releaseNativeReadEnds?: () => Promise<void>;
  readonly finished: Promise<void>;

  constructor(private config: Configuration) {
    if (config.sessionId !== undefined && !/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(config.sessionId)) throw new Error('SDK session id must be a UUID');
    if (!isAbsolute(config.executable) || !isAbsolute(config.cwd)) throw new Error('Executable and cwd must be absolute');
    this.permissions = new PermissionCallbacks(config.observe);
    const env = { ...config.env, CLAUDE_CODE_STARTUP_FAILURE_RESULTS: '1' };
    const sessionId = config.sessionId?.toLowerCase();
    const profile = config.orchestrator;
    if (profile?.reasoning_effort && !['low', 'medium', 'high', 'xhigh', 'max'].includes(profile.reasoning_effort)) throw new Error('The selected Claude reasoning effort is unsupported by this SDK');
    const permissions: Options = profile
      ? {
          sessionId,
          model: profile.model,
          ...(profile.reasoning_effort ? { effort: profile.reasoning_effort as NonNullable<Options['effort']> } : {}),
          tools: profile.tools,
          disallowedTools: profile.disallowed_tools,
          mcpServers: Object.fromEntries(Object.entries(profile.mcp_servers).map(([name, server]) => [name, { type: 'stdio' as const, command: server.command, args: server.args }])),
          strictMcpConfig: true,
          plugins: profile.plugins.map((path) => ({ type: 'local' as const, path })),
          settings: profile.settings as Options['settings'],
          permissionMode: 'auto',
          permissionPrompts: 'none',
          extraArgs: { restricted: null },
        }
      : {
          ...(config.resume ? { resume: sessionId } : { sessionId }),
          tools: { type: 'preset', preset: 'claude_code' },
          permissionMode: 'auto',
          permissionPrompts: 'host',
          canUseTool: this.permissions.request,
          onElicitation: this.permissions.elicitation,
          hooks: { PreToolUse: [{ matcher: 'Bash', hooks: [this.permissions.folder] }] },
          ...(config.worker && {
            plugins: config.worker.plugins.map((path) => ({ type: 'local' as const, path })),
            settings: config.worker.settings as Options['settings'],
            ...(config.worker.model && { model: config.worker.model }),
            ...(config.worker.effort && { effort: config.worker.effort }),
            ...(config.worker.disallowed_tools?.length && { disallowedTools: config.worker.disallowed_tools }),
          }),
        };
    const options: Options = {
      ...permissions,
      cwd: config.cwd,
      pathToClaudeCodeExecutable: config.executable,
      env,
      settingSources: ['user', 'project', 'local'],
      systemPrompt: { type: 'preset', preset: 'claude_code' },
      abortController: this.controller,
      spawnClaudeCodeProcess: (options) => {
        config.beforeSpawn?.();
        const native = config.spawn ? undefined : spawn(options.command, options.args, { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'pipe'] });
        let stderr = Buffer.alloc(0);
        native?.stderr.on('data', (chunk: Buffer) => { stderr = Buffer.concat([stderr, chunk]).subarray(-16_384); });
        const child = native ?? config.spawn!(options);
        this.child = child;
        this.childEnded = new Promise<void>((resolve) => { child.once('exit', () => resolve()); child.once('error', () => { if (child.pid === undefined) resolve(); }); });
        if (native) {
          const closed = new Promise<void>((resolve) => native.once('close', () => {
            try {
              if (stderr.length) this.config.observe({ type: 'sdk', kind: 'diagnostic', native: {}, payload: { source: 'stderr', text: safeDiagnostic(stderr.toString('utf8')), truncated_at_bytes: 16_384 }, failures: [] });
            } finally { resolve(); }
          }));
          this.releaseNativeReadEnds = async () => {
            native.stdout.destroy();
            native.stderr.destroy();
            await closed;
          };
        }
        child.on('exit', (exit_code, signal) => {
          this.config.observe({ type: 'process_exited', exit_code, signal });
        });
        return child;
      },
    };
    this.query = query({ prompt: this.input, options });
    this.finished = this.consume();
  }

  offer(inputId: string, prompt: string): void {
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(inputId)) throw new Error('SDK input id must be a UUID');
    inputId = inputId.toLowerCase();
    if (this.inputs.size >= 64) throw new Error('Outstanding input capacity exceeded');
    if (this.inputs.has(inputId)) throw new Error('Duplicate outstanding input');
    this.input.offer({ type: 'user', uuid: inputId as NonNullable<SDKUserMessage['uuid']>, session_id: '', parent_tool_use_id: null, message: { role: 'user', content: prompt } });
    this.inputs.add(inputId);
    this.turnOpen = true;
  }
  respond(request: DecisionRequest, decision: Decision): boolean {
    return this.permissions.respond(request, decision);
  }
  interrupt(): ReturnType<Query['interrupt']> { return this.query.interrupt(); }
  /** A turn still running ends as an interrupted turn, so closing never cuts Claude mid-turn; Claude exiting first also settles it. */
  async interruptTurn(): Promise<void> {
    if (!this.turnOpen || this.closing) return;
    await Promise.race([this.query.interrupt(), this.finished]);
  }
  /** Ends Claude's process: the query closes, and each step after it comes only while the process
   * has not exited: SIGTERM, then SIGKILL. Resolves once the session finished. */
  async forceStop(): Promise<void> {
    this.close();
    for (const signal of ['SIGTERM', 'SIGKILL'] as const) {
      const exited = await Promise.race([this.childEnded.then(() => true), new Promise<false>((resolve) => setTimeout(resolve, STOP_GRACE_MS, false).unref())]);
      if (exited) break;
      this.child?.kill(signal);
    }
    await this.finished;
  }
  supportedModels(): ReturnType<Query['supportedModels']> { return this.query.supportedModels(); }
  setModel(model: string): Promise<void> { return this.query.setModel(model); }
  setPermissionMode(mode: 'default' | 'plan' | 'dontAsk'): Promise<void> { return this.query.setPermissionMode(mode); }
  mcpServerStatus(): ReturnType<Query['mcpServerStatus']> { return this.query.mcpServerStatus(); }
  reconnectMcpServer(server: string): Promise<void> { return this.query.reconnectMcpServer(server); }
  /** The SDK sends `mcp_authenticate` through this method, which its published types leave out. */
  mcpAuthenticate(server: string): Promise<unknown> {
    const authenticate = (this.query as unknown as { mcpAuthenticate?: (server: string) => Promise<unknown> }).mcpAuthenticate;
    if (typeof authenticate !== 'function') return Promise.reject(new Error('This Agent SDK cannot sign in to MCP servers'));
    return authenticate.call(this.query, server);
  }
  /** The SDK sends `mcp_clear_auth` through this method, which its published types leave out. */
  mcpClearAuth(server: string): Promise<unknown> {
    const clear = (this.query as unknown as { mcpClearAuth?: (server: string) => Promise<unknown> }).mcpClearAuth;
    if (typeof clear !== 'function') return Promise.reject(new Error('This Agent SDK cannot sign out of MCP servers'));
    return clear.call(this.query, server);
  }
  close(): void {
    this.closing = true;
    this.input.close();
    this.permissions.close();
    this.query.close();
  }
  private async consume(): Promise<void> {
    try {
      for await (const message of this.query) {
        const payload = message as unknown as Record<string, unknown>;
        if (payload.type === 'result') this.seenResult = true;
        if (payload.parent_tool_use_id === undefined || payload.parent_tool_use_id === null) {
          if (payload.type === 'result') this.turnOpen = false;
          else if (payload.type === 'assistant') this.turnOpen = true;
        }
        // This bounds outstanding sends; domain consumption remains a Rust observation decision.
        if ((payload.type === 'assistant' || payload.type === 'result')
            && (payload.parent_tool_use_id === undefined || payload.parent_tool_use_id === null)) {
          const ids = payload.user_message_uuids;
          const singular = payload.user_message_uuid;
          if (Array.isArray(ids) && ids.length > 0 && ids.length <= 64
              && typeof singular === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(singular)
              && ids.every((id): id is string => typeof id === 'string' && /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i.test(id))
              && new Set(ids.map((id) => id.toLowerCase())).size === ids.length
              && ids.some((id) => id.toLowerCase() === singular.toLowerCase())) {
            for (const id of ids) this.inputs.delete(id.toLowerCase());
          }
        }
        this.config.observe(classify(payload));
        if (payload.type === 'system' && payload.subtype === 'init' && !this.accountReported) this.reportAccount(payload.session_id);
        if (this.config.orchestrator && payload.type === 'result' && this.inputs.size === 0) this.input.close();
      }
      if (!this.closing && (!this.seenResult || this.inputs.size > 0)) this.diagnostic('eof_without_result', { unresolved_input_ids: [...this.inputs] });
    } catch (error) {
      this.diagnostic(this.closing ? 'aborted' : 'iterator_error', { name: error instanceof Error ? error.name : 'Error', message: safeDiagnostic(error instanceof Error ? error.message : String(error)), unresolved_input_ids: [...this.inputs] }, this.closing ? 'control' : 'iterator');
    } finally {
      try {
        this.input.close();
        this.permissions.close();
        this.query.close();
      } finally {
        await this.childEnded;
        await this.releaseNativeReadEnds?.();
      }
    }
  }
  /** The plan of the account Claude signed in with, as the session initialized: Rust attributes it to the execution's account. */
  private reportAccount(sessionId: unknown): void {
    this.accountReported = true;
    this.query.initializationResult().then((initialized) => {
      const subscriptionType = initialized.account?.subscriptionType;
      if (this.closing || typeof sessionId !== 'string' || typeof subscriptionType !== 'string') return;
      this.config.observe({ type: 'sdk', kind: 'status', native: {}, payload: { type: 'account', session_id: sessionId, account: { subscriptionType } }, failures: [] });
    }, () => { /* A session that never initialized reports no account. */ });
  }
  private diagnostic(code: string, details: unknown, surface = 'iterator'): void {
    details = fitValue(details, MAX_DIAGNOSTIC_BYTES);
    this.config.observe({ type: 'sdk', kind: 'diagnostic', native: {}, payload: details, failures: [{ surface, code, details }] });
  }
}
