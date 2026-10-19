import { randomBytes } from 'node:crypto';
import { createServer, type Server, type Socket } from 'node:net';
import { Channel } from './channel.js';
import { assertBootstrap, prepareSocket, secureSocket, releaseBootstrap, type Bootstrap } from './bootstrap.js';
import { commandDigest, executionSegments, MAX_MCP_SERVERS, PROTOCOL_VERSION, type Action, type ControlResult, type Frame } from './frame.js';
import { Journal, type Command, type CommandRecord } from './journal.js';
import { proofMatches, hostProof, type Attach } from './ownership.js';
import { Session, type Observation } from './session.js';

type Lane = { channel: Channel; nonce: string; generation?: number; sentCursor: number; receipts: Map<string, string>; flushing: boolean; pending: number };
export const HOST_CAPABILITIES = ['streaming_input', 'decisions', 'owner_attach', 'commit_ack', 'interrupt', 'set_model', 'set_permission_mode', 'mcp_status'] as const;
export type Capability = typeof HOST_CAPABILITIES[number];
type Hooks = { beforeEffect?: () => Promise<void>; beforeSpawn?: () => void; observation?: (observation: Observation) => void; capabilities?: readonly Capability[] };
const REQUIRED: Record<Action['type'], Capability | undefined> = {
  start: 'streaming_input', offer: 'streaming_input', respond: 'decisions', interrupt: 'interrupt', close: undefined,
  set_model: 'set_model', set_permission_mode: 'set_permission_mode', mcp_status: 'mcp_status',
};
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
const MAX_CONNECTIONS = 8;
const MAX_COMMANDS = 256;
const MAX_QUEUED = 64;

/** Private Claude transport owner and technical outbox; business decisions stay in Rust. */
export class PersistentHost {
  private server: Server;
  private lanes = new Set<Lane>();
  private owner?: Lane;
  private session?: Session;
  private operations: Promise<void> = Promise.resolve();
  private journal: Journal;
  private unavailable = false;
  private stopping = false;
  private stopPromise?: Promise<void>;
  private capabilities: ReadonlySet<Capability>;
  constructor(private config: Bootstrap, private hooks: Hooks = {}) {
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
    process.umask(0o077);
    await new Promise<void>((resolve, reject) => {
      const failed = (error: Error) => reject(error);
      this.server.once('error', failed);
      this.server.listen({ path: this.config.socket_path, readableAll: false, writableAll: false }, () => {
        this.server.off('error', failed);
        if (this.server.address() !== this.config.socket_path) { reject(new Error('Owned socket address mismatch')); return; }
        try { secureSocket(this.config); resolve(); } catch (error) { reject(error); }
      });
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
    const lane: Lane = { channel, nonce: randomBytes(32).toString('hex'), sentCursor: this.journal.acknowledged, receipts: new Map(), flushing: false, pending: 0 };
    this.lanes.add(lane);
    socket.on('close', () => { this.lanes.delete(lane); if (this.owner === lane) this.owner = undefined; });
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
        this.persist(frame); return;
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
    const digest = commandDigest(raw); const previous = this.journal.commands.get(command.command_id);
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
    const required = REQUIRED[command.action.type];
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
          this.session.offer(command.action.input_id, command.action.prompt);
          this.receipt(record, { status: 'accepted', phase: 'offered' });
          break;
        }
        case 'offer':
          // A host launched after its execution's previous host ended starts the native session on
          // its first input: resumed when that host's Claude had one, never by a turn of its own.
          if (!this.session && this.config.journal_base !== undefined) this.session = this.open(lane);
          if (!this.session) throw new Error('Session absent');
          this.current(lane); this.session.offer(command.action.input_id, command.action.prompt);
          this.receipt(record, { status: 'accepted', phase: 'offered' }); break;
        case 'respond': {
          this.current(lane);
          const { request_id, epoch, kind, tool_use_id, decision } = command.action;
          if (!this.session?.respond({ request_id, epoch, kind, tool_use_id }, decision)) {
            this.receipt(record, { status: 'rejected', code: 'stale_request', message: 'Claude is not waiting for this request' }); break;
          }
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        }
        case 'interrupt':
          if (!this.session) throw new Error('Session absent');
          this.current(lane); await this.session.interrupt();
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
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
          const servers = (await session.mcpServerStatus()).flatMap((server) => {
            const name = bounded(server.name); const status = bounded(server.status); const error = bounded(server.error);
            return name && status ? [{ name, status, ...(error ? { error } : {}) }] : [];
          }).slice(0, MAX_MCP_SERVERS);
          this.current(lane);
          const result: ControlResult = { control: 'mcp_status', command_id: command.command_id, servers };
          this.observe({ type: 'sdk', kind: 'control_result', native: {}, payload: result, failures: [] });
          if (this.unavailable) throw new Error('Control result not durable');
          this.receipt(record, { status: 'accepted', phase: 'applied' }); break;
        }
      }
    } catch (error) {
      // Once SDK handoff may have occurred, keep admission as uncertainty rather than fabricate rejection.
      this.diagnostic('sdk_action_uncertain ' + ((error as { message?: string })?.message ?? String(error)));
      throw new Error('SDK action uncertain');
    }
  }
  private open(lane: Lane): Session {
    const session = new Session({ ...this.config.native, sessionId: this.config.native.session_id, orchestrator: this.config.orchestrator, worker: this.config.worker,
      beforeSpawn: () => { this.hooks.beforeSpawn?.(); this.current(lane); },
      observe: (observation) => this.observe(observation),
    });
    void session.finished.catch(() => this.storageUncertain());
    return session;
  }
  private controlled(record: CommandRecord): Session | undefined {
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
      this.persist({ protocol_version: PROTOCOL_VERSION, body: { type: 'event', fence: this.config.fence, execution: this.config.execution, cursor, observation } } as Frame);
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
    void (async () => {
      try {
        for (;;) {
          this.current(lane);
          let sent = false;
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
    })().catch(() => lane.channel.socket.destroy());
  }
  private diagnostic(code: string): void { process.stderr.write(`Alondra host: ${code}\n`); }
  stop(): Promise<void> {
    this.stopPromise ??= this.stopOwned();
    return this.stopPromise;
  }
  private async stopOwned(): Promise<void> {
    this.stopping = true;
    const failures: unknown[] = [];
    try {
      try { this.session?.close(); } catch (error) { failures.push(error); }
      try { await this.session?.finished; } catch (error) { failures.push(error); }
    } finally {
      for (const lane of this.lanes) lane.channel.socket.destroy();
      try { await this.operations; } catch (error) { failures.push(error); }
      try { await new Promise<void>((resolve) => this.server.close(() => resolve())); } catch (error) { failures.push(error); }
      try { this.journal.close(); } catch (error) { failures.push(error); }
      try { releaseBootstrap(this.config); } catch (error) { failures.push(error); }
    }
    if (failures.length) throw new AggregateError(failures, 'Owned host cleanup failed');
  }
}
