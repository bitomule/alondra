import { appendFileSync, closeSync, constants, existsSync, fstatSync, fsyncSync, openSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { commandDigest, encode, parsePayload, MAX_FRAME_BYTES, type Frame } from './frame.js';

export type Command = Extract<Frame['body'], { type: 'command' }>;
export type Event = Extract<Frame['body'], { type: 'event' }>;
export type Receipt = Extract<Frame['body'], { type: 'receipt' }>;
export type CommandRecord = { command: Command; raw: Buffer; digest: string; receipt?: Frame & { body: Receipt } };
const JOURNAL_BYTES = 16 * 1024 * 1024;

/** Only technical commands/receipts/observations/ACKs; no Task or card authority. */
export class Journal {
  private fd: number;
  private bytes = 0;
  readonly commands = new Map<string, CommandRecord>();
  readonly events: Map<number, Frame & { body: Event }> = new Map();
  ownerGeneration = 0;
  cursor = 0;
  acknowledged = 0;
  readonly recovering: boolean;
  readonly path: string;
  readonly uncertainPath: string;
  /** `base` is the cursor the execution's previous host ended at: this journal's events continue from it. */
  constructor(private directory: string, private validateBinding: (frame: Frame) => void, base = 0) {
    this.cursor = base; this.acknowledged = base;
    this.path = join(directory, 'outbox'); this.uncertainPath = join(directory, 'uncertain');
    this.fd = openSync(this.path, constants.O_CREAT | constants.O_RDWR | constants.O_APPEND | constants.O_NOFOLLOW, 0o600);
    const stat = fstatSync(this.fd);
    if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid?.() || (stat.mode & 0o777) !== 0o600 || stat.size > JOURNAL_BYTES) { closeSync(this.fd); throw new Error('Private journal refused'); }
    this.bytes = stat.size;
    try {
      const raw = readFileSync(this.fd); let offset = 0;
      while (offset < raw.length) {
        if (raw.length - offset < 4) throw new Error('Truncated journal');
        const length = raw.readUInt32BE(offset); offset += 4;
        if (!length || length > MAX_FRAME_BYTES || length > raw.length - offset) throw new Error('Invalid journal record');
        const payload = raw.subarray(offset, offset + length); this.apply(parsePayload(payload), payload); offset += length;
      }
    } catch (error) { closeSync(this.fd); throw error; }
    this.recovering = this.commands.size > 0 || existsSync(this.uncertainPath);
    this.syncDirectory();
  }
  append(frame: Frame, original?: Buffer): void {
    const bytes = original ? Buffer.concat([this.header(original.length), original]) : encode(frame);
    if (this.bytes + bytes.length > JOURNAL_BYTES) throw new Error('Journal capacity');
    const decoded = parsePayload(bytes.subarray(4));
    this.validateRecord(decoded, bytes.subarray(4));
    appendFileSync(this.fd, bytes); fsyncSync(this.fd); this.bytes += bytes.length;
    this.apply(decoded, bytes.subarray(4));
  }
  private header(length: number): Buffer { const value = Buffer.alloc(4); value.writeUInt32BE(length); return value; }
  private validateRecord(frame: Frame, raw: Buffer): void {
    this.validateBinding(frame);
    const body = frame.body;
    if (body.type === 'command') {
      const previous = this.commands.get(body.command_id);
      if ((previous && previous.digest !== commandDigest(raw)) || (!previous && this.commands.size >= 256)) throw new Error('Command journal conflict or capacity');
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
      if (body.owner_generation !== this.ownerGeneration || body.cursor < this.acknowledged || body.cursor > this.cursor) throw new Error('ACK journal fence or cursor');
    } else throw new Error('Non-durable journal record');
  }
  private apply(frame: Frame, raw: Buffer): void {
    this.validateRecord(frame, raw);
    const body = frame.body;
    switch (body.type) {
      case 'command': {
        const digest = commandDigest(raw); const previous = this.commands.get(body.command_id);
        if (previous && previous.digest !== digest) throw new Error('Journal command conflict');
        if (!previous) this.commands.set(body.command_id, { command: body, raw: Buffer.from(raw), digest });
        break;
      }
      case 'receipt': {
        const previous = this.commands.get(body.command_id);
        if (!previous || previous.digest !== body.command_sha256) throw new Error('Journal receipt conflict');
        previous.receipt = frame as Frame & { body: Receipt }; break;
      }
      case 'event':
        if (body.cursor !== this.cursor + 1) throw new Error('Journal cursor gap');
        this.cursor = body.cursor; this.events.set(body.cursor, frame as Frame & { body: Event }); break;
      case 'attached':
        if (body.owner_generation <= this.ownerGeneration) throw new Error('Journal owner regression');
        this.ownerGeneration = body.owner_generation; break;
      case 'acknowledge':
        if (body.cursor < this.acknowledged || body.cursor > this.cursor) throw new Error('Journal ACK cursor');
        this.acknowledged = body.cursor;
        for (const cursor of this.events.keys()) if (cursor <= body.cursor) this.events.delete(cursor);
        break;
      default: throw new Error('Non-durable journal record');
    }
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
