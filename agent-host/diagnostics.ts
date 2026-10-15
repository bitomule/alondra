/** All authenticated control uses Unix IPC; stdout has one diagnostic bootstrap wakeup. */
export const LISTENING_HINT = 'Alondra host: socket listening\n';
let readySink: ((chunk: string, callback: (error?: Error | null) => void) => boolean) | undefined;
export async function listeningHint(): Promise<void> {
  const sink = readySink;
  if (!sink) throw new Error('Diagnostic sink not installed');
  await new Promise<void>((resolve) => {
    try { sink(LISTENING_HINT, () => resolve()); } catch { resolve(); }
  });
}
export function boundDiagnostics(): void {
  for (const stream of [process.stdout, process.stderr]) {
    const write = stream.write.bind(stream);
    if (stream === process.stdout) readySink = (chunk, callback) => write(chunk, callback);
    let pending = 0;
    stream.on('error', () => {});
    stream.write = ((chunk: string | Uint8Array, encoding?: BufferEncoding | ((error?: Error | null) => void), callback?: (error?: Error | null) => void): boolean => {
      const done = typeof encoding === 'function' ? encoding : callback;
      const size = typeof chunk === 'string' ? Buffer.byteLength(chunk, typeof encoding === 'string' ? encoding : undefined) : chunk.byteLength;
      if (stream.destroyed || size > 16384 || pending + size > 16384) { if (done) queueMicrotask(() => done()); return true; }
      pending += size;
      try {
        return write(chunk, typeof encoding === 'string' ? encoding : undefined, (error) => { pending -= size; done?.(error); });
      } catch { pending -= size; if (done) queueMicrotask(() => done()); return true; }
    }) as typeof stream.write;
  }
}
