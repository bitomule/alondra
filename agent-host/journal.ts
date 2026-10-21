import { appendFileSync, closeSync, constants, existsSync, fstatSync, fsyncSync, openSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { commandDigest, encode, parsePayload, MAX_FRAME_BYTES, type Frame } from './frame.js';

export type Command = Extract<Frame['body'], { type: 'command' }>;
export type Event = Extract<Frame['body'], { type: 'event' }>;
export type Receipt = Extract<Frame['body'], { type: 'receipt' }>;
export type CommandRecord = { command: Command; raw: Buffer; digest: string; receipt?: Frame & { body: Receipt }; receipts: Buffer[]; finalAt?: number };
/** A command the owner can no longer send again: what is left of it answers a copy that arrives anyway. */
export type SettledCommand = { digest: string; receipt?: Frame & { body: Receipt } };
export type Limits = { retainedBytes: number; compactAt: number };
/** The evidence the daemon has not acknowledged yet, at most: past it the host takes no more rather than lose any. */
const RETAINED_BYTES = 64 * 1024 * 1024;
/** A journal this large whose acknowledged part is at least half is rewritten with only what is still needed. */
const COMPACT_AT = 4 * 1024 * 1024;
const COMMANDS = 256;
const FINAL_PHASES = new Set(['offered', 'applied']);
const framed = (payload: Buffer): Buffer => { const header = Buffer.alloc(4); header.writeUInt32BE(payload.length); return Buffer.concat([header, payload]); };

/** Only technical commands/receipts/observations/ACKs; no Task or card authority. */
export class Journal {
  private fd: number;
  private bytes = 0;
  /** Bytes of the commands, receipts and events a compaction keeps. */
  private retained = 0;
  private sequence = 0;
  private loading = true;
  private worked = false;
  private attached?: Buffer;
  private acknowledgement?: { raw: Buffer; attached: Buffer };
  private eventBytes = new Map<number, Buffer>();
  private limits: Limits;
  readonly commands = new Map<string, CommandRecord>();
  readonly settled = new Map<string, SettledCommand>();
  readonly events: Map<number, Frame & { body: Event }> = new Map();
  ownerGeneration = 0;
  cursor = 0;
  acknowledged = 0;
  readonly recovering: boolean;
  readonly path: string;
  readonly uncertainPath: string;
  private readonly nextPath: string;
  /** `base` is the cursor the execution's previous host ended at: this journal's events continue from it. */
  constructor(private directory: string, private validateBinding: (frame: Frame) => void, base = 0, limits: Partial<Limits> = {}) {
    this.limits = { retainedBytes: RETAINED_BYTES, compactAt: COMPACT_AT, ...limits };
    this.cursor = base; this.acknowledged = base;
    this.path = join(directory, 'outbox'); this.uncertainPath = join(directory, 'uncertain'); this.nextPath = join(directory, 'outbox.next');
    rmSync(this.nextPath, { force: true });
    this.fd = this.open();
    this.bytes = fstatSync(this.fd).size;
    try {
      const raw = readFileSync(this.fd); let offset = 0;
      while (offset < raw.length) {
        if (raw.length - offset < 4) throw new Error('Truncated journal');
        const length = raw.readUInt32BE(offset); offset += 4;
        if (!length || length > MAX_FRAME_BYTES || length > raw.length - offset) throw new Error('Invalid journal record');
        const payload = raw.subarray(offset, offset + length); this.apply(parsePayload(payload), payload); offset += length;
      }
    } catch (error) { closeSync(this.fd); throw error; }
    this.loading = false;
    this.recovering = this.worked || existsSync(this.uncertainPath);
    this.syncDirectory();
  }
  private open(): number {
    const fd = openSync(this.path, constants.O_CREAT | constants.O_RDWR | constants.O_APPEND | constants.O_NOFOLLOW, 0o600);
    const stat = fstatSync(fd);
    if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid?.() || (stat.mode & 0o777) !== 0o600 || stat.size > 2 * this.limits.retainedBytes + this.limits.compactAt + 2 * (MAX_FRAME_BYTES + 4)) { closeSync(fd); throw new Error('Private journal refused'); }
    return fd;
  }
  append(frame: Frame, original?: Buffer): void {
    const bytes = original ? framed(original) : encode(frame);
    const decoded = parsePayload(bytes.subarray(4));
    if (['command', 'receipt', 'event'].includes(decoded.body.type) && this.retained + bytes.length > this.limits.retainedBytes) throw new Error('Journal capacity');
    this.validateRecord(decoded, bytes.subarray(4));
    appendFileSync(this.fd, bytes); fsyncSync(this.fd); this.bytes += bytes.length;
    this.apply(decoded, bytes.subarray(4));
    if (decoded.body.type === 'acknowledge' && this.bytes > this.limits.compactAt && 2 * this.retained <= this.bytes) this.compact();
  }
  private validateRecord(frame: Frame, raw: Buffer): void {
    this.validateBinding(frame);
    const body = frame.body;
    if (body.type === 'command') {
      const previous = this.commands.get(body.command_id) ?? this.settled.get(body.command_id);
      if ((previous && previous.digest !== commandDigest(raw)) || (!previous && this.commands.size >= COMMANDS)) throw new Error('Command journal conflict or capacity');
    } else if (body.type === 'receipt') {
      const previous = this.commands.get(body.command_id);
      if (!previous || previous.digest !== body.command_sha256) throw new Error('Receipt journal identity');
      const earlier = previous.receipt?.body.outcome;
      if (!earlier && (body.outcome.status !== 'accepted' || body.outcome.phase !== 'admitted')) throw new Error('Receipt admission missing');
      if (earlier && JSON.stringify(earlier) !== JSON.stringify(body.outcome)) {
        if (earlier.status === 'rejected') throw new Error('Rejected receipt is terminal');
        if (body.outcome.status === 'rejected') { if (earlier.phase !== 'admitted') throw new Error('Possible handoff cannot become rejected'); }
        else if (['admitted', 'offered', 'applied'].indexOf(body.outcome.phase) <= ['admitted', 'offered', 'applied'].indexOf(earlier.phase)) throw new Error('Receipt phase regression');
      }
    } else if (body.type === 'event') {
      if (body.cursor !== this.cursor + 1) throw new Error('Event journal cursor');
    } else if (body.type === 'attached') {
      if (body.owner_generation <= this.ownerGeneration) throw new Error('Owner journal generation');
    } else if (body.type === 'acknowledge') {
      if (body.owner_generation !== this.ownerGeneration || body.cursor < this.acknowledged || (body.cursor > this.cursor && !this.compactedBase())) throw new Error('ACK journal fence or cursor');
    } else throw new Error('Non-durable journal record');
  }
  /** A compacted journal starts at the ACK it was compacted up to: no event of it comes before. */
  private compactedBase(): boolean { return this.loading && this.events.size === 0 && this.cursor === this.acknowledged; }
  private apply(frame: Frame, raw: Buffer): void {
    this.validateRecord(frame, raw);
    this.sequence += 1;
    const body = frame.body;
    switch (body.type) {
      case 'command': {
        this.settle();
        const digest = commandDigest(raw); const previous = this.commands.get(body.command_id) ?? this.settled.get(body.command_id);
        if (previous && previous.digest !== digest) throw new Error('Journal command conflict');
        if (!previous) { this.commands.set(body.command_id, { command: body, raw: Buffer.from(raw), digest, receipts: [] }); this.retained += raw.length + 4; }
        this.worked = this.worked || this.loading;
        break;
      }
      case 'receipt': {
        const previous = this.commands.get(body.command_id);
        if (!previous || previous.digest !== body.command_sha256) throw new Error('Journal receipt conflict');
        previous.receipt = frame as Frame & { body: Receipt };
        previous.receipts.push(framed(Buffer.from(raw))); this.retained += raw.length + 4;
        if (body.outcome.status === 'rejected' || FINAL_PHASES.has(body.outcome.phase)) previous.finalAt = this.sequence;
        break;
      }
      case 'event':
        if (body.cursor !== this.cursor + 1) throw new Error('Journal cursor gap');
        this.cursor = body.cursor; this.events.set(body.cursor, frame as Frame & { body: Event });
        this.eventBytes.set(body.cursor, framed(Buffer.from(raw))); this.retained += raw.length + 4; break;
      case 'attached':
        if (body.owner_generation <= this.ownerGeneration) throw new Error('Journal owner regression');
        this.ownerGeneration = body.owner_generation; this.attached = framed(Buffer.from(raw)); break;
      case 'acknowledge':
        if (body.cursor < this.acknowledged || (body.cursor > this.cursor && !this.compactedBase())) throw new Error('Journal ACK cursor');
        if (body.cursor > this.cursor) this.cursor = body.cursor;
        this.acknowledged = body.cursor;
        for (const cursor of this.events.keys()) if (cursor <= body.cursor) { this.events.delete(cursor); this.retained -= this.eventBytes.get(cursor)!.length; this.eventBytes.delete(cursor); }
        this.acknowledgement = { raw: framed(Buffer.from(raw)), attached: this.attached! };
        this.settle();
        this.worked = this.worked || this.loading;
        break;
      default: throw new Error('Non-durable journal record');
    }
  }
  /**
   * The owner sends nothing after a command before it has stored that command's outcome, and it
   * only sends again a command whose outcome it never stored: once the host has a later frame of
   * the owner, a command with a final receipt is never sent again and leaves the journal.
   */
  private settle(): void {
    for (const [id, record] of this.commands) {
      if (record.finalAt === undefined || record.finalAt >= this.sequence) continue;
      this.commands.delete(id);
      this.settled.set(id, { digest: record.digest, receipt: record.receipt });
      this.retained -= record.raw.length + 4 + record.receipts.reduce((sum, receipt) => sum + receipt.length, 0);
    }
  }
  /** Rewrites the journal with only what is still needed, replacing it at once: a cut rewrite leaves the old one. */
  compact(): void {
    const parts: Buffer[] = [];
    if (this.acknowledgement) parts.push(this.acknowledgement.attached, this.acknowledgement.raw);
    if (this.attached && this.attached !== this.acknowledgement?.attached) parts.push(this.attached);
    for (const record of this.commands.values()) parts.push(framed(record.raw), ...record.receipts);
    parts.push(...this.eventBytes.values());
    const fd = openSync(this.nextPath, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
    try {
      for (const part of parts) appendFileSync(fd, part);
      fsyncSync(fd);
    } catch (error) { closeSync(fd); rmSync(this.nextPath, { force: true }); throw error; }
    closeSync(fd);
    renameSync(this.nextPath, this.path);
    this.syncDirectory();
    const compacted = this.open();
    closeSync(this.fd);
    this.fd = compacted;
    this.bytes = fstatSync(this.fd).size;
  }
  markUncertain(): void {
    if (!existsSync(this.uncertainPath)) {
      const fd = openSync(this.uncertainPath, constants.O_CREAT | constants.O_EXCL | constants.O_WRONLY | constants.O_NOFOLLOW, 0o600);
      try { writeFileSync(fd, 'Technical evidence storage uncertain\n'); fsyncSync(fd); } finally { closeSync(fd); }
      this.syncDirectory();
    }
  }
  private syncDirectory(): void { const fd = openSync(this.directory, constants.O_RDONLY); try { fsyncSync(fd); } finally { closeSync(fd); } }
  close(): void { closeSync(this.fd); }
}
