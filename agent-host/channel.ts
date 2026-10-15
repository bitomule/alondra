import { Socket } from 'node:net';
import { encode, parsePayload, MAX_FRAME_BYTES, type Frame } from './frame.js';

/** One bounded decoder and one ordered writer per authenticated transport lane. */
export class Channel {
  private header = Buffer.alloc(4);
  private headerBytes = 0;
  private payload?: Buffer;
  private payloadBytes = 0;
  private writer: Promise<void> = Promise.resolve();
  private pendingBytes = 0;
  constructor(readonly socket: Socket, receive: (frame: Frame, raw: Buffer) => void, failed: () => void) {
    socket.on('error', failed);
    socket.on('data', (chunk: Buffer) => {
      try {
        let offset = 0;
        while (offset < chunk.length) {
          if (!this.payload) {
            const count = Math.min(4 - this.headerBytes, chunk.length - offset);
            chunk.copy(this.header, this.headerBytes, offset, offset + count);
            this.headerBytes += count; offset += count;
            if (this.headerBytes !== 4) continue;
            const length = this.header.readUInt32BE();
            if (length === 0 || length > MAX_FRAME_BYTES) throw new Error('Frame length');
            this.payload = Buffer.alloc(length); this.payloadBytes = 0;
          }
          const count = Math.min(this.payload.length - this.payloadBytes, chunk.length - offset);
          chunk.copy(this.payload, this.payloadBytes, offset, offset + count);
          this.payloadBytes += count; offset += count;
          if (this.payloadBytes === this.payload.length) {
            const raw = this.payload;
            this.payload = undefined; this.headerBytes = 0; this.payloadBytes = 0;
            receive(parsePayload(raw), raw);
          }
        }
      } catch { failed(); socket.destroy(); }
    });
    socket.on('end', () => { if (this.headerBytes || this.payload) failed(); });
  }
  write(frame: Frame): Promise<void> {
    if (this.socket.destroyed) return Promise.reject(new Error('Transport disconnected'));
    const bytes = encode(frame);
    if (this.pendingBytes + bytes.length > 4 * MAX_FRAME_BYTES) return Promise.reject(new Error('Transport output capacity'));
    this.pendingBytes += bytes.length;
    const result = this.writer.then(() => new Promise<void>((resolve, reject) => {
      if (this.socket.destroyed) { reject(new Error('Transport disconnected')); return; }
      const close = () => { cleanup(); reject(new Error('Transport disconnected')); };
      const cleanup = () => { this.socket.off('close', close); };
      this.socket.once('close', close);
      this.socket.write(bytes, (error) => { cleanup(); if (error) reject(error); else resolve(); });
    }));
    this.writer = result.catch(() => { this.socket.destroy(); });
    return result.finally(() => { this.pendingBytes -= bytes.length; });
  }
}
