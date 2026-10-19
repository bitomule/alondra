import { query, type Options, type Query, type SDKUserMessage, type PermissionResult, type SpawnOptions, type SpawnedProcess } from '@anthropic-ai/claude-agent-sdk/core';
import { spawn, type ChildProcess } from 'node:child_process';
import { isAbsolute } from 'node:path';
import type { OrchestratorProfile } from './bootstrap.js';

export type Failure = { surface: string; code: string; details: unknown };
export type Observation =
  | { type: 'sdk'; kind: string; native: Record<string, string>; payload: unknown; failures: Failure[] }
  | { type: 'decision_requested'; request_id: string; tool_use_id: string; kind: 'permission'; payload: unknown }
  | { type: 'process_exited'; exit_code: number | null; signal: string | null };
export type Configuration = {
  executable: string;
  cwd: string;
  env: Readonly<Record<string, string | undefined>>;
  observe: (observation: Observation) => void;
  beforeSpawn?: () => void;
  sessionId?: string;
  /** One turn of Alondra's orchestrator: its own tools, MCP and permissions, and nothing after its result. */
  orchestrator?: OrchestratorProfile;
  // Test override must return a local ChildProcess: a PID proves spawn occurred.
  spawn?: (options: SpawnOptions) => SpawnedProcess & Pick<ChildProcess, 'pid'>;
};

class Input implements AsyncIterable<SDKUserMessage> {
  private items: SDKUserMessage[] = [];
  private wake?: () => void;
  private closed = false;
  offer(message: SDKUserMessage): void {
    if (this.closed) throw new Error('Input is closed');
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

const systemMessages = new Set(['init', 'compact_boundary', 'status', 'api_retry', 'local_command_output', 'hook_started', 'hook_progress', 'hook_response', 'plugin_install', 'task_notification', 'task_started', 'task_updated', 'task_progress', 'background_tasks_changed', 'thinking_tokens', 'session_state_changed', 'worker_shutting_down', 'commands_changed', 'notification', 'files_persisted', 'memory_recall', 'elicitation_complete', 'permission_denied', 'informational', 'conversation_reset', 'mirror_error', 'model_refusal_fallback', 'model_refusal_no_fallback', 'control_request_progress']);
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

export class PermissionCallbacks {
  private pending = new Map<string, (result: PermissionResult) => void>();
  private closed = false;
  constructor(private observe: Configuration['observe']) {}
  request: NonNullable<Options['canUseTool']> = async (tool, input, request) => {
    if (this.closed || request.signal.aborted) return { behavior: 'deny', message: 'Permission request cancelled' };
    if (this.pending.has(request.requestId)) return { behavior: 'deny', message: 'Duplicate permission request' };
    if (this.pending.size >= 32) return { behavior: 'deny', message: 'Permission request capacity exceeded' };
    return new Promise<PermissionResult>((resolve) => {
      const finish = (result: PermissionResult) => {
        if (this.pending.get(request.requestId) !== finish) return;
        request.signal.removeEventListener('abort', cancel);
        this.pending.delete(request.requestId);
        resolve(result);
      };
      const cancel = () => finish({ behavior: 'deny', message: 'Permission request cancelled' });
      this.pending.set(request.requestId, finish);
      request.signal.addEventListener('abort', cancel, { once: true });
      try {
        const metadata = Object.fromEntries(Object.entries(request).filter(([key, value]) => key !== 'signal' && value !== undefined));
        this.observe({ type: 'decision_requested', request_id: request.requestId, tool_use_id: request.toolUseID, kind: 'permission', payload: { tool, input, request: metadata } });
      } catch {
        finish({ behavior: 'deny', message: 'Permission delivery failed' });
      }
    });
  };
  respond(requestId: string, result: PermissionResult): void {
    const finish = this.pending.get(requestId);
    if (!finish) throw new Error('Unknown or expired permission request');
    finish(result);
  }
  close(): void {
    this.closed = true;
    for (const finish of [...this.pending.values()]) finish({ behavior: 'deny', message: 'Session ended' });
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
  private closing = false;
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
    const permissions: Options = profile
      ? {
          ...(profile.resume ? { resume: sessionId } : { sessionId }),
          model: profile.model,
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
          sessionId,
          tools: { type: 'preset', preset: 'claude_code' },
          permissionMode: 'default',
          permissionPrompts: 'host',
          canUseTool: this.permissions.request,
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
  }
  respond(requestId: string, result: PermissionResult): void {
    this.permissions.respond(requestId, result);
  }
  interrupt(): ReturnType<Query['interrupt']> { return this.query.interrupt(); }
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
  private diagnostic(code: string, details: unknown, surface = 'iterator'): void {
    this.config.observe({ type: 'sdk', kind: 'diagnostic', native: {}, payload: details, failures: [{ surface, code, details }] });
  }
}
