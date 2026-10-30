import { randomBytes } from 'node:crypto';
import { createServer, type Server, type Socket } from 'node:net';
import { Channel } from './channel.js';
import { assertBootstrap, prepareSocket, secureSocket, releaseBootstrap, type Bootstrap } from './bootstrap.js';
import { ACTION_CAPABILITIES, commandDigest, executionSegments, fitObservation, framingProblem, MAX_MCP_SERVERS, MAX_MCP_URL_BYTES, MAX_SIGN_IN_URL_BYTES, PROTOCOL_VERSION, HOST_CAPABILITIES, type Capability, type ControlResult, type Frame, type McpServer, type SignIn } from './frame.js';
import { Journal, type Command, type CommandRecord } from './journal.js';
import { proofMatches, hostProof, type Attach } from './ownership.js';
import { ControlNotApplied, InputNotOffered, type HostSession, type Observation, type SessionFactory } from './host-session.js';

type Lane = { channel: Channel; nonce: string; generation?: number; sentCursor: number; receipts: Map<string, string>; flushing: boolean; flushed: Promise<void>; pending: number };
export { HOST_CAPABILITIES, type Capability } from './frame.js';
export type Hooks = { beforeEffect?: () => Promise<void>; beforeSpawn?: () => void; observation?: (observation: Observation) => void; capabilities?: readonly Capability[]; endsWithFailedSession?: boolean; endsWithSession?: boolean; endHostGroup?: () => void };
const NATIVE_MODES = { default: 'default', plan: 'plan', dont_ask: 'dontAsk' } as const;
const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/gu;
function bounded(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  let clipped = '';
  for (const character of value.replace(LONE_SURROGATE, '\uFFFD').replaceAll('\0', '')) {
    if (Buffer.byteLength(clipped + character) > 256) break;
    clipped += character;
  }
  return clipped || undefined;
}
function failure(error: unknown): string {
  return bounded((error as { message?: unknown })?.message) ?? bounded(String(error)) ?? 'Claude gave no reason';
}
/** A remote server's address as Alondra may keep it: scheme, host and path, never credentials or a query. */
function address(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  try {
    const url = new URL(value);
    if (url.protocol !== 'https:' && url.protocol !== 'http:') return undefined;
    const kept = `${url.protocol}//${url.host}${url.pathname}`;
    return Buffer.byteLength(kept) <= MAX_MCP_URL_BYTES && !/[?#@]/.test(kept) ? kept : undefined;
  } catch { return undefined; }
}
type NativeServer = { name?: unknown; status?: unknown; error?: unknown; source?: unknown; config?: { type?: unknown; command?: unknown; url?: unknown } };
export function listed(servers: readonly NativeServer[]): McpServer[] {
  return servers.flatMap((server) => {
    const name = bounded(server.name); const status = bounded(server.status);
    if (!name || !status) return [];
    const error = bounded(server.error); const source = bounded(server.source);
    const transport = bounded(server.config?.type) ?? (typeof server.config?.command === 'string' ? 'stdio' : undefined);
    const url = address(server.config?.url);
    return [{ name, status, ...(error ? { error } : {}), ...(source ? { source } : {}), ...(transport ? { transport } : {}), ...(url ? { url } : {}) }];
  }).slice(0, MAX_MCP_SERVERS);
}
/** How the sign-in Claude opened goes on. Claude keeps the flow and what it returns. */
function signIn(response: unknown): SignIn {
  const native = (response ?? {}) as { authUrl?: unknown; requiresUserAction?: unknown; callbackPort?: unknown; state?: unknown };
  if (native.requiresUserAction === false) return { outcome: 'connected' };
  const url = typeof native.authUrl === 'string' && Buffer.byteLength(native.authUrl) <= MAX_SIGN_IN_URL_BYTES ? native.authUrl : undefined;
  if (!url || !/^(https:\/\/|http:\/\/localhost|http:\/\/127\.0\.0\.1)/.test(url)) return { outcome: 'refused', error: 'Claude gave no sign-in address Alondra can open' };
  const port = native.callbackPort; const state = bounded(native.state);
  const callback = typeof port === 'number' && Number.isInteger(port) && port > 0 && port <= 65535 && state ? { callback_port: port, state } : {};
  return { outcome: 'browser', url, ...callback };
}
const MAX_CONNECTIONS = 8;
const MAX_COMMANDS = 256;
const MAX_QUEUED = 64;

/** Private Claude transport owner and technical outbox; business decisions stay in Rust. */
export class PersistentHost {
  private server: Server;
  private lanes = new Set<Lane>();
  private owner?: Lane;
  private session?: HostSession;
  private operations: Promise<void> = Promise.resolve();
  private journal: Journal;
  private unavailable = false;
  private stopping = false;
  private sessionEnd?: { cursor: number; acknowledged: number; resolve: () => void; progress?: () => void };
  private stopPromise?: Promise<void>;
  private capabilities: ReadonlySet<Capability>;
  constructor(private config: Bootstrap, private createSession: SessionFactory, private hooks: Hooks = {}) {
    assertBootstrap(config);
    this.capabilities = new Set(hooks.capabilities ?? HOST_CAPABILITIES);
    this.journal = new Journal(config.state_dir, (frame) => {
      const body = frame.body;
      if (!('execution' in body)) throw new Error('Journal identity missing');
      this.binding(body);
      if ('host_instance_id' in body && body.host_instance_id !== config.host_instance_id) throw new Error('Journal host instance mismatch');
    }, config.journal_base);
    for (const record of this.journal.commands.values()) this.binding(record.command);
    for (const frame of this.journal.events.values()) this.binding(frame.body);
    this.server = createServer((socket) => this.connection(socket));
    this.server.maxConnections = MAX_CONNECTIONS;
  }
  async listen(): Promise<void> {
    prepareSocket(this.config);
    await new Promise<void>((resolve, reject) => {
      const failed = (error: Error) => reject(error);
      this.server.once('error', failed);
      // Node binds the Unix socket synchronously inside listen(): the private umask covers only that bind,
      // so Claude and every command it runs keep the user's own umask.
      const user = process.umask(0o077);
      try {
        this.server.listen({ path: this.config.socket_path, readableAll: false, writableAll: false }, () => {
          this.server.off('error', failed);
          if (this.server.address() !== this.config.socket_path) { reject(new Error('Owned socket address mismatch')); return; }
          try { secureSocket(this.config); resolve(); } catch (error) { reject(error); }
        });
      } finally { process.umask(user); }
    });
  }
  private identity() { return { fence: this.config.fence, execution: this.config.execution, host_instance_id: this.config.host_instance_id }; }
  private binding(body: { fence: Bootstrap['fence']; execution: Bootstrap['execution'] }): void {
    for (const key of ['profile_id', 'account_id', 'data_epoch', 'host_id'] as const) if (body.fence[key] !== this.config.fence[key]) throw new Error('Host fence mismatch');
    const expected = executionSegments(this.config.execution); const actual = executionSegments(body.execution);
    if (expected.length !== actual.length || expected.some((segment, at) => segment !== actual[at])) throw new Error('Host execution mismatch');
  }
  private current(lane: Lane): void {
    assertBootstrap(this.config);
    if (this.stopping || lane !== this.owner || lane.generation !== this.journal.ownerGeneration || lane.channel.socket.destroyed) throw new Error('Owner lane revoked');
  }
  private connection(socket: Socket): void {
    if (this.lanes.size >= MAX_CONNECTIONS) { socket.destroy(); return; }
    const channel = new Channel(socket, (frame, raw) => this.receive(lane, frame, raw), () => this.diagnostic('transport_rejected'));
    const lane: Lane = { channel, nonce: randomBytes(32).toString('hex'), sentCursor: this.journal.acknowledged, receipts: new Map(), flushing: false, flushed: Promise.resolve(), pending: 0 };
    this.lanes.add(lane);
    socket.on('close', () => { this.lanes.delete(lane); if (this.owner === lane) { this.owner = undefined; this.releaseSessionEnd(); } });
    void (async () => {
      await channel.write({ protocol_version: PROTOCOL_VERSION, body: { type: 'hello', fence: this.config.fence, protocol_versions: [1], capabilities: [...this.capabilities], host_release: this.config.manifest.host_release, host_sha256: this.config.manifest.host_sha256, sdk_version: this.config.manifest.sdk_version } });
      await channel.write({ protocol_version: PROTOCOL_VERSION, body: { type: 'challenge', ...this.identity(), nonce: lane.nonce, executable_sha256: this.config.manifest.executable_sha256 } });
    })().catch(() => socket.destroy());
  }
  private receive(lane: Lane, frame: Frame, raw: Buffer): void {
    try {
      const body = frame.body;
      if (body.type === 'attach') { this.attach(lane, body); return; }
      this.current(lane);
      if (body.type === 'acknowledge') {
        this.binding(body);
        if (body.host_instance_id !== this.config.host_instance_id || body.owner_generation !== lane.generation || body.cursor > lane.sentCursor || body.cursor > this.journal.cursor || body.cursor < this.journal.acknowledged) throw new Error('ACK rejected');
        this.persist(frame);
        const ended = this.sessionEnd;
        if (ended) {
          if (body.cursor >= ended.cursor) this.releaseSessionEnd();
          else if (body.cursor > ended.acknowledged) { ended.acknowledged = body.cursor; ended.progress?.(); }
        }
        return;
      }
      if (body.type !== 'command') throw new Error('Unexpected owner message');
      this.binding(body);
      if (++lane.pending > MAX_QUEUED) throw new Error('Command queue capacity');
      this.operations = this.operations.then(async () => {
        try { await this.command(lane, body, raw); }
        catch { lane.channel.socket.destroy(); this.diagnostic('owner_command_uncertain'); }
        finally { lane.pending--; }
      });
    } catch { lane.channel.socket.destroy(); this.diagnostic('owner_message_rejected'); }
  }
  private attach(lane: Lane, attach: Attach): void {
    if (this.stopping) throw new Error('Host stopping');
    this.binding(attach);
    if (attach.host_instance_id !== this.config.host_instance_id || attach.nonce !== lane.nonce
        || attach.host_release !== this.config.manifest.host_release || attach.host_sha256 !== this.config.manifest.host_sha256
        || attach.executable_sha256 !== this.config.manifest.executable_sha256 || attach.sdk_version !== this.config.manifest.sdk_version
        || attach.owner_generation <= this.journal.ownerGeneration || !proofMatches(Buffer.from(this.config.control_secret, 'hex'), attach)) throw new Error('Owner proof rejected');
    const grant: Frame = { protocol_version: PROTOCOL_VERSION, body: { type: 'attached', ...this.identity(), owner_generation: attach.owner_generation, proof: hostProof(Buffer.from(this.config.control_secret, 'hex'), attach) } };
    this.persist(grant);
    const previous = this.owner;
    lane.generation = attach.owner_generation; lane.nonce = ''; this.owner = lane;
    if (previous && previous !== lane) previous.channel.socket.destroy();
    void (async () => { await lane.channel.write(grant); this.flush(lane); })().catch(() => lane.channel.socket.destroy());
  }
  private async command(lane: Lane, command: Command, raw: Buffer): Promise<void> {
    this.current(lane); this.binding(command);
    const digest = commandDigest(raw); const previous = this.journal.commands.get(command.command_id) ?? this.journal.settled.get(command.command_id);
    if (previous) {
      if (previous.digest !== digest) throw new Error('Command identity conflict');
      // A re-sent command is the owner reconciling after a crash. Return its definitive retained
      // receipt whatever its phase, so an admitted-only receipt (the action already ran and its
      // outcome is unknown) reaches the owner: it is never permission to offer again.
      const receipt = previous.receipt;
      if (receipt) { this.current(lane); await lane.channel.write(receipt); }
      return;
    }
    if (this.journal.commands.size >= MAX_COMMANDS || command.deadline_ms < Date.now()) throw new Error('Command admission refused');
    this.persist({ protocol_version: PROTOCOL_VERSION, body: command }, raw);
    const record = this.journal.commands.get(command.command_id)!;
    this.receipt(record, { status: 'accepted', phase: 'admitted' });
    if (this.journal.recovering || this.unavailable) {
      this.receipt(record, { status: 'rejected', code: 'host_reconciliation_required', message: 'Prior native work and effects need positive reconciliation' }); return;
    }
    if (this.config.orchestrator && command.action.type !== 'start' && command.action.type !== 'interrupt' && command.action.type !== 'close') {
      this.receipt(record, { status: 'rejected', code: 'single_turn', message: 'An orchestrator batch is one turn: a later input waits for the next batch' }); return;
    }
    const required = ACTION_CAPABILITIES[command.action.type];
    if (required && !this.capabilities.has(required)) {
      this.receipt(record, { status: 'rejected', code: 'unsupported', message: `This host does not support ${command.action.type}` }); return;
    }
    await this.hooks.beforeEffect?.();
    this.current(lane);
    try {
      switch (command.action.type) {
        case 'start': {
          if (this.session || command.action.cwd !== this.config.native.cwd || this.config.journal_base !== undefined) throw new Error('Owned session configuration refused');
          this.current(lane);
          this.session = this.open(lane);
          this.current(lane);
          const offered=this.session.offer(command.action.input_id, command.action.prompt);
          if(offered){await offered;this.current(lane);}
          this.receipt(record, { status: 'accepted', phase: 'offered' });
          break;
        }
        case 'offer': {
          // A host launched after its execution's previous host ended starts the native session on
          // its first input: resumed when that host's Claude had one, never by a turn of its own.
          if (!this.session && this.config.journal_base !== undefined) this.session = this.open(lane);
          if (!this.session) throw new Error('Session absent');
          this.current(lane);const offered=this.session.offer(command.action.input_id, command.action.prompt);
          if(offered){await offered;this.current(lane);}
          this.receipt(record, { status: 'accepted', phase: 'offered' }); break;
        }
        case 'respond': {
          this.current(lane);
          const { request_id, epoch, kind, tool_use_id, decision, asked } = command.action;
          if (!this.session?.respond({ request_id, epoch, kind, tool_use_id, asked }, decision)) {
            this.receipt(record, { status: 'rejected', code: 'stale_request', message: 'Claude is not waiting for this request' }); break;
          }
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        }
        case 'interrupt':
          if (!this.session) throw new Error('Session absent');
          this.current(lane); await this.session.interrupt();
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        case 'force_stop': {
          const session = this.controlled(record); if (!session) break;
          this.current(lane); await session.forceStop();
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        }
        case 'close':
          if (!this.session) throw new Error('Session absent');
          this.current(lane); this.session.close(); await this.session.finished;
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        case 'set_model': {
          const session = this.controlled(record); if (!session) break;
          const model = command.action.model;
          const offered = await session.supportedModels();
          this.current(lane);
          if (!offered.some((entry) => entry.value === model)) {
            this.receipt(record, { status: 'rejected', code: 'unsupported_model', message: 'Claude does not offer this model' }); break;
          }
          await session.setModel(model);
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        }
        case 'set_permission_mode': {
          const session = this.controlled(record); if (!session) break;
          this.current(lane); await session.setPermissionMode(NATIVE_MODES[command.action.mode]);
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        }
        case 'mcp_status': {
          const session = this.controlled(record); if (!session) break;
          this.current(lane);
          const servers = listed(await session.mcpServerStatus());
          this.current(lane);
          this.answer(record, { control: 'mcp_status', command_id: command.command_id, servers }); break;
        }
        case 'mcp_reconnect': {
          const session = this.controlled(record); if (!session) break;
          this.current(lane);
          const server = command.action.server;
          const error = await session.reconnectMcpServer(server).then(() => undefined, failure);
          this.current(lane);
          const servers = listed(await session.mcpServerStatus());
          this.current(lane);
          this.answer(record, { control: 'mcp_reconnect', command_id: command.command_id, server, ...(error ? { error } : {}), servers }); break;
        }
        case 'mcp_authenticate': {
          const session = this.controlled(record); if (!session) break;
          this.current(lane);
          const server = command.action.server;
          const sign_in = await session.mcpAuthenticate(server).then(signIn, (error): SignIn => ({ outcome: 'refused', error: failure(error) }));
          this.current(lane);
          this.answer(record, { control: 'mcp_authenticate', command_id: command.command_id, server, sign_in }); break;
        }
        case 'mcp_clear_auth': {
          const session = this.controlled(record); if (!session) break;
          this.current(lane);
          const server = command.action.server;
          const error = await session.mcpClearAuth(server).then(() => undefined, failure);
          this.current(lane);
          const servers = listed(await session.mcpServerStatus());
          this.current(lane);
          this.answer(record, { control: 'mcp_clear_auth', command_id: command.command_id, server, ...(error ? { error } : {}), servers }); break;
        }
      }
    } catch (error) {
      if (command.action.type === 'set_model' && error instanceof ControlNotApplied && error.code === 'native_code_cells_active') {
        this.current(lane);
        this.receipt(record, { status: 'rejected', code: error.code, message: 'The native code cells must finish before changing the model' });
        return;
      }
      if (error instanceof InputNotOffered && (command.action.type === 'start' || command.action.type === 'offer')) {
        this.current(lane);
        this.receipt(record, { status: 'rejected', code: error.code, message: 'The native account policy refused this input before submission' });
        return;
      }
      // Once SDK handoff may have occurred, keep admission as uncertainty rather than fabricate rejection.
      this.diagnostic('sdk_action_uncertain ' + ((error as { message?: string })?.message ?? String(error)));
      throw new Error('SDK action uncertain');
    }
  }
  private open(lane: Lane): HostSession {
    const session = this.createSession({ ...this.config.native, stateDirectory: this.config.state_dir, processGeneration: this.config.host_instance_id, accountId: this.config.fence.account_id, hostRelease: this.config.manifest.host_release, sessionId: this.config.native.session_id, orchestrator: this.config.orchestrator, worker: this.config.worker,
      beforeSpawn: () => { this.hooks.beforeSpawn?.(); this.current(lane); },
      observe: (observation) => this.observe(observation),
    });
    void session.finished.then(() => {
      if ((this.hooks.endsWithSession || this.hooks.endsWithFailedSession && session.failure) && !this.stopping) {
        this.unavailable = true;
        if (session.failure) process.exitCode = 1;
        const terminal = this.journal.cursor;
        let acknowledged = Promise.resolve();
        if (this.hooks.endsWithSession && this.owner && this.journal.acknowledged < terminal) {
          acknowledged = new Promise<void>((resolve) => { this.sessionEnd = { cursor: terminal, acknowledged: this.journal.acknowledged, resolve }; });
        }
        let deadline: ReturnType<typeof setTimeout> | undefined;
        let exitDeadline: ReturnType<typeof setTimeout> | undefined;
        const bounded = new Promise<void>((resolve) => {
          const expire = () => { if (this.sessionEnd) this.sessionEnd.progress = undefined; resolve(); };
          const renew = () => {
            clearTimeout(deadline);
            deadline = setTimeout(expire, session.failure ? 1000 : 5000);
            clearTimeout(exitDeadline);
            exitDeadline = setTimeout(() => this.hooks.endHostGroup?.(), session.failure ? 2000 : 6000);
          };
          renew();
          if (!session.failure && this.sessionEnd) this.sessionEnd.progress = renew;
        });
        void (async () => {
          try { await Promise.race([Promise.all([this.operations.then(() => this.owner?.flushed).catch(() => {}), acknowledged]), bounded]); }
          finally {
            clearTimeout(deadline);
            try { await this.stop(); }
            finally { clearTimeout(exitDeadline); }
          }
        })().catch(() => { process.exitCode = 1; });
      }
    }, () => this.storageUncertain());
    return session;
  }
  /** The control's result is evidence before the receipt that closes the command. */
  private answer(record: CommandRecord, result: ControlResult): void {
    this.observe({ type: 'sdk', kind: 'control_result', native: {}, payload: result, failures: [] });
    if (this.unavailable) throw new Error('Control result not durable');
    this.receipt(record, { status: 'accepted', phase: 'applied' });
  }
  private controlled(record: CommandRecord): HostSession | undefined {
    if (!this.session) this.receipt(record, { status: 'rejected', code: 'session_absent', message: 'No Claude session is running on this host' });
    return this.session;
  }
  private receipt(record: CommandRecord, outcome: Extract<Frame['body'], { type: 'receipt' }>['outcome']): void {
    this.persist({ protocol_version: PROTOCOL_VERSION, body: { type: 'receipt', fence: this.config.fence, execution: this.config.execution, command_id: record.command.command_id, command_sha256: record.digest, outcome } });
    if (this.owner) this.flush(this.owner);
  }
  private observe(observation: Observation): number | undefined {
    try {
      const cursor = this.journal.cursor + 1;
      const frame = (observation: Observation) => ({ protocol_version: PROTOCOL_VERSION, body: { type: 'event', fence: this.config.fence, execution: this.config.execution, cursor, observation } } as Frame);
      if (observation.type === 'sdk') observation = fitObservation(observation, (candidate) => framingProblem(frame(candidate)));
      this.persist(frame(observation));
      try { this.hooks.observation?.(observation); } catch { this.diagnostic('diagnostic_tap_failed'); }
      if (this.owner) this.flush(this.owner);
      return cursor;
    } catch { this.storageUncertain(); return undefined; }
  }
  private persist(frame: Frame, original?: Buffer): void {
    try { this.journal.append(frame, original); } catch { this.storageUncertain(); throw new Error('Technical evidence not durable'); }
  }
  private storageUncertain(): void {
    this.unavailable = true;
    try { this.journal.markUncertain(); } catch { /* I/O failure cannot be converted into healthy persisted evidence. */ }
    this.owner?.channel.socket.destroy(); this.diagnostic('storage_uncertain');
  }
  private flush(lane: Lane): void {
    if (lane.flushing) return;
    lane.flushing = true;
    lane.flushed = (async () => {
      try {
        for (;;) {
          this.current(lane);
          let sent = false;
          for (const id of lane.receipts.keys()) if (!this.journal.commands.has(id)) lane.receipts.delete(id);
          for (const record of this.journal.commands.values()) {
            const frame = record.receipt;
            if (!frame) continue;
            // The admitted receipt is an internal uncertainty marker, not a reconciliation answer:
            // it is retained and re-sent only when the owner re-sends its command, never flushed.
            if (frame.body.outcome.status === 'accepted' && frame.body.outcome.phase === 'admitted') continue;
            const identity = JSON.stringify(frame.body.outcome);
            if (lane.receipts.get(record.command.command_id) === identity) continue;
            await lane.channel.write(frame); this.current(lane);
            lane.receipts.set(record.command.command_id, identity); sent = true;
          }
          for (const [cursor, frame] of this.journal.events) {
            if (cursor <= lane.sentCursor) continue;
            await lane.channel.write(frame); this.current(lane);
            lane.sentCursor = cursor; sent = true;
          }
          if (!sent) break;
        }
      } finally { lane.flushing = false; }
    })().catch(() => { lane.channel.socket.destroy(); });
  }
  private diagnostic(code: string): void { process.stderr.write(`Alondra host: ${code}\n`); }
  private releaseSessionEnd(): void {
    const ended = this.sessionEnd;
    this.sessionEnd = undefined;
    ended?.resolve();
  }
  /** With `interruptTurn`, a turn Claude is running ends as an interrupted turn before the session closes. */
  stop(interruptTurn = false): Promise<void> {
    this.releaseSessionEnd();
    this.stopPromise ??= this.stopOwned(interruptTurn);
    return this.stopPromise;
  }
  private async stopOwned(interruptTurn: boolean): Promise<void> {
    this.stopping = true;
    const failures: unknown[] = [];
    try {
      if (interruptTurn) {
        try { await this.session?.interruptTurn(); } catch { this.diagnostic('interrupt_before_close_failed'); }
      }
      try { this.session?.close(); } catch (error) { failures.push(error); }
      try { await this.session?.finished; } catch (error) { failures.push(error); }
    } finally {
      for (const lane of this.lanes) lane.channel.socket.destroy();
      try { await this.operations; } catch (error) { failures.push(error); }
      try { await new Promise<void>((resolve) => this.server.close(() => resolve())); } catch (error) { failures.push(error); }
      try { this.journal.close(); } catch (error) { failures.push(error); }
      // A Codex host keeps ownership until its whole old group is terminated in the kernel.
      try {
        if (this.hooks.endHostGroup) this.hooks.endHostGroup();
        else releaseBootstrap(this.config);
      } catch (error) { failures.push(error); }
    }
    if (failures.length) throw new AggregateError(failures, 'Owned host cleanup failed');
  }
}
