import { constants, watch, type FSWatcher, type Stats } from 'node:fs';
import { lstat, open, realpath, type FileHandle } from 'node:fs/promises';
import { isAbsolute, join, relative, resolve, sep } from 'node:path';

type Json = Record<string, unknown>;
export type CodeCellRolloutItem = { threadId: string; turnId: string; item: Json };
export type RolloutOptions = {
  path: string;
  codexHome: string;
  threadId: string;
  onItem: (value: CodeCellRolloutItem) => void | Promise<void>;
  onError: (error: CodexRolloutError) => void;
  anchor?: { currentChildTurns: ReadonlySet<string> };
  limits?: { lineBytes?: number; chunkBytes?: number; drainBytes?: number };
};
export class CodexRolloutError extends Error {
  constructor(readonly code: string) { super(`Codex code-cell history unavailable (${code}); native work remains unresolved.`); }
}
const object = (value: unknown): Json | undefined => value !== null && typeof value === 'object' && !Array.isArray(value) ? value as Json : undefined;
const identity = (value: unknown): string | undefined => typeof value === 'string' && value.length > 0 && value.length <= 256 && !/[\u0000-\u001f]/.test(value) ? value : undefined;
const defaultNamespace = (value: unknown): boolean => value === undefined || value === null || value === 'functions';
const same = (a: Stats, b: Stats): boolean => a.dev === b.dev && a.ino === b.ino;

export class CodexRolloutFollower {
  private watcher?: FSWatcher;
  private watchPending = false;
  private watchDrain?: Promise<void>;
  private queue: Promise<void> = Promise.resolve();
  private closing?: Promise<void>;
  private failure?: CodexRolloutError;
  private closed = false;
  private offset: number;
  private pending = Buffer.alloc(0);
  private prefix = Buffer.alloc(0);
  private prefixAt = 0;
  private baseline: number;
  private readonly childTurns?: Set<string>;
  private readonly seenTurns = new Set<string>();
  private readonly calls = new Map<string, 'exec' | 'wait'>();
  private readonly lineBytes: number;
  private readonly chunkBytes: number;
  private readonly drainBytes: number;

  private constructor(private readonly options: RolloutOptions, private readonly file: FileHandle, private readonly inode: Stats, private readonly canonical: string, initialSize: number) {
    this.childTurns = options.anchor ? new Set(options.anchor.currentChildTurns) : undefined;
    this.offset = this.childTurns ? 0 : initialSize;
    this.baseline = this.childTurns ? initialSize : 0;
    this.lineBytes = options.limits?.lineBytes ?? 2 * 1024 * 1024;
    this.chunkBytes = options.limits?.chunkBytes ?? 64 * 1024;
    this.drainBytes = options.limits?.drainBytes ?? 32 * 1024 * 1024;
  }

  static async open(options: RolloutOptions): Promise<CodexRolloutFollower> {
    const refuse = (code: string): never => { throw new CodexRolloutError(code); };
    if (!isAbsolute(options.path) || !isAbsolute(options.codexHome) || !identity(options.threadId) || options.path.includes('\0') || options.codexHome.includes('\0')) refuse('invalid_path');
    for (const value of Object.values(options.limits ?? {})) if (value !== undefined && (!Number.isSafeInteger(value) || value < 1 || value > 32 * 1024 * 1024)) refuse('invalid_bounds');
    if (options.anchor && (!options.anchor.currentChildTurns.size || options.anchor.currentChildTurns.size > 1024 || [...options.anchor.currentChildTurns].some((turn) => !identity(turn)))) refuse('missing_child_boundary');
    let file: FileHandle | undefined;
    let follower: CodexRolloutFollower | undefined;
    try {
      const requested = resolve(options.path);
      const home = await realpath(options.codexHome);
      const sessionsRoots = [resolve(options.codexHome, 'sessions'), join(home, 'sessions')];
      const suffix = sessionsRoots.map((sessions) => relative(sessions, requested)).find((suffix) => suffix && suffix !== '..' && !suffix.startsWith('..' + sep) && !isAbsolute(suffix));
      if (!suffix) throw new CodexRolloutError('outside_profile');
      const canonical = join(home, 'sessions', suffix);
      const pieces = ['sessions', ...suffix.split(sep)];
      let current = home;
      for (const [index, piece] of pieces.entries()) {
        current = join(current, piece);
        const node = await lstat(current);
        if (node.isSymbolicLink() || node.uid !== process.getuid?.() || (index + 1 < pieces.length ? !node.isDirectory() : !node.isFile() || node.nlink !== 1)) refuse('unsafe_path');
      }
      if (await realpath(requested) !== canonical) refuse('changed_path');
      const before = await lstat(requested);
      file = await open(requested, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
      const after = await file.stat();
      if (!same(before, after) || !after.isFile() || after.nlink !== 1 || after.uid !== process.getuid?.() || await realpath(requested) !== canonical || !Number.isSafeInteger(after.size)) refuse('changed_file');
      follower = new CodexRolloutFollower(options, file, after, canonical, after.size);
      await follower.validateSession(after.size);
      await follower.capturePrefix();
      follower.watcher = watch(requested, (event) => {
        if (follower!.closed) return;
        if (event === 'rename') follower!.latch('changed_file');
        else follower!.scheduleWatchDrain();
      });
      follower.watcher.on('error', () => { if (!follower!.closed) follower!.latch('watch_failed'); });
      await follower.drain();
      return follower;
    } catch (error) {
      if (follower) await follower.close();
      else await file?.close();
      throw error instanceof CodexRolloutError ? error : new CodexRolloutError('open_failed');
    }
  }

  drain(): Promise<void> {
    const result = this.queue.then(async () => {
      if (this.closed) throw new CodexRolloutError('closed');
      if (this.failure) throw this.failure;
      try { await this.readAvailable(); }
      catch (error) { throw this.latch(error instanceof CodexRolloutError ? error.code : 'read_failed'); }
    });
    this.queue = result.catch(() => {});
    return result;
  }

  close(): Promise<void> {
    if (this.closing) return this.closing;
    this.closed = true;
    this.watcher?.close();
    this.closing = Promise.all([this.queue, this.watchDrain]).then(async () => { await this.file.close(); });
    return this.closing;
  }

  private scheduleWatchDrain(): void {
    this.watchPending = true;
    if (this.watchDrain) return;
    this.watchDrain = Promise.resolve().then(async () => {
      while (this.watchPending && !this.closed && !this.failure) {
        this.watchPending = false;
        await this.drain();
      }
    }).catch(() => {}).finally(() => { this.watchDrain = undefined; });
  }

  private latch(code: string): CodexRolloutError {
    if (!this.failure) {
      this.failure = new CodexRolloutError(code);
      try { this.options.onError(this.failure); } catch { /* The original observation failure stays authoritative. */ }
    }
    return this.failure;
  }

  private async validateSession(size: number): Promise<void> {
    if (!size) throw new CodexRolloutError('missing_session');
    const end = Buffer.alloc(1);
    if ((await this.file.read(end, 0, 1, size - 1)).bytesRead !== 1 || end[0] !== 10) throw new CodexRolloutError('incomplete_anchor');
    let bytes = Buffer.alloc(0); let offset = 0;
    while (offset < size && bytes.indexOf(10) < 0) {
      const next = Buffer.alloc(Math.min(this.chunkBytes, size - offset, this.lineBytes + 1 - bytes.length));
      if (!next.length) throw new CodexRolloutError('line_too_large');
      const read = (await this.file.read(next, 0, next.length, offset)).bytesRead;
      if (!read) throw new CodexRolloutError('truncated');
      bytes = Buffer.concat([bytes, next.subarray(0, read)]); offset += read;
    }
    const newline = bytes.indexOf(10);
    if (newline < 0 || newline > this.lineBytes) throw new CodexRolloutError('invalid_session');
    const line = this.parse(bytes.subarray(0, newline));
    if (line.type !== 'session_meta' || object(line.payload)?.id !== this.options.threadId) throw new CodexRolloutError('wrong_thread');
  }

  private async validateFile(): Promise<Stats> {
    const node = await lstat(this.options.path);
    const live = await this.file.stat();
    if (node.isSymbolicLink() || !node.isFile() || node.nlink !== 1 || live.nlink !== 1 || node.uid !== process.getuid?.() || !same(node, this.inode) || !same(live, this.inode) || await realpath(this.options.path) !== this.canonical) throw new CodexRolloutError('changed_file');
    if (!Number.isSafeInteger(live.size) || live.size < this.offset) throw new CodexRolloutError('truncated');
    if (this.prefix.length) {
      const bytes = Buffer.alloc(this.prefix.length);
      const read = (await this.file.read(bytes, 0, bytes.length, this.prefixAt)).bytesRead;
      if (read !== bytes.length || !bytes.equals(this.prefix)) throw new CodexRolloutError('changed_prefix');
    }
    return live;
  }

  private async capturePrefix(): Promise<void> {
    const length = Math.min(256, this.offset);
    this.prefixAt = this.offset - length;
    this.prefix = Buffer.alloc(length);
    if ((await this.file.read(this.prefix, 0, length, this.prefixAt)).bytesRead !== length) throw new CodexRolloutError('truncated');
  }

  private async readAvailable(): Promise<void> {
    const end = (await this.validateFile()).size;
    if (end - this.offset > this.drainBytes) throw new CodexRolloutError('drain_too_large');
    while (this.offset < end && !this.closed) {
      const next = Buffer.alloc(Math.min(this.chunkBytes, end - this.offset));
      const read = (await this.file.read(next, 0, next.length, this.offset)).bytesRead;
      if (!read) throw new CodexRolloutError('truncated');
      this.offset += read;
      this.pending = Buffer.concat([this.pending, next.subarray(0, read)]);
      let newline;
      while ((newline = this.pending.indexOf(10)) >= 0) {
        if (newline > this.lineBytes) throw new CodexRolloutError('line_too_large');
        const bytes = this.pending.subarray(0, newline);
        this.pending = this.pending.subarray(newline + 1);
        const lineEnd = this.offset - this.pending.length;
        await this.consume(this.parse(bytes), this.baseline > 0 && lineEnd <= this.baseline);
      }
      if (this.pending.length > this.lineBytes) throw new CodexRolloutError('line_too_large');
    }
    if (this.baseline && this.offset >= this.baseline) {
      if ([...this.childTurns!].some((turn) => !this.seenTurns.has(turn))) throw new CodexRolloutError('missing_child_boundary');
      this.baseline = 0;
    }
    await this.validateFile();
    await this.capturePrefix();
  }

  private parse(bytes: Buffer): Json {
    try {
      const value = object(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(bytes)));
      if (!value) throw new Error();
      return value;
    } catch { throw new CodexRolloutError('invalid_record'); }
  }

  private async consume(line: Json, baseline: boolean): Promise<void> {
    const item = object(line.payload);
    if (line.type === 'session_meta' && item?.id !== this.options.threadId) throw new CodexRolloutError('wrong_thread');
    const turn = identity(object(item?.internal_chat_message_metadata_passthrough)?.turn_id ?? item?.turn_id);
    if (baseline && turn && this.childTurns!.has(turn)) this.seenTurns.add(turn);
    if (line.type !== 'response_item' || !item) return;
    if (baseline && !turn && ((item.type === 'custom_tool_call' && item.name === 'exec') || (item.type === 'function_call' && item.name === 'wait'))) throw new CodexRolloutError('missing_identity');
    if (baseline && (!turn || !this.childTurns!.has(turn))) return;
    const callId = identity(item.call_id);
    const callKey = turn && callId ? turn + '\0' + callId : undefined;
    const nativeExec = item.type === 'custom_tool_call' && item.name === 'exec' && defaultNamespace(item.namespace);
    const nativeWait = item.type === 'function_call' && item.name === 'wait' && defaultNamespace(item.namespace);
    const output = item.type === 'custom_tool_call_output' || item.type === 'function_call_output';
    const knownOutput = output && callKey && this.calls.has(callKey);
    if (!nativeExec && !nativeWait && !knownOutput) {
      if (output && defaultNamespace(item.namespace) && (item.name === 'exec' || item.name === 'wait')) throw new CodexRolloutError('unmatched_output');
      return;
    }
    if (!callId || !turn || !callKey) throw new CodexRolloutError('missing_identity');
    if (!defaultNamespace(item.namespace) || (item.name !== undefined && item.name !== null && item.name !== 'exec' && item.name !== 'wait')) throw new CodexRolloutError('invalid_tool_identity');
    const projected: Json = { type: item.type, call_id: callId, ...(typeof item.namespace === 'string' ? { namespace: item.namespace } : {}), ...(typeof item.name === 'string' ? { name: item.name } : {}) };
    if (nativeExec || nativeWait) {
      if (this.calls.has(callKey)) throw new CodexRolloutError('duplicate_call');
      if (this.calls.size >= 8192) throw new CodexRolloutError('call_limit');
      this.calls.set(callKey, nativeExec ? 'exec' : 'wait');
      if (nativeWait) {
        let args;
        try { args = object(JSON.parse(String(item.arguments))); } catch { throw new CodexRolloutError('invalid_wait'); }
        const cell = identity(args?.cell_id);
        if (!cell) throw new CodexRolloutError('invalid_wait');
        projected.arguments = JSON.stringify({ cell_id: cell });
      }
    } else {
      if (item.type !== (this.calls.get(callKey) === 'exec' ? 'custom_tool_call_output' : 'function_call_output')) throw new CodexRolloutError('invalid_output_kind');
      const first = typeof item.output === 'string' ? item.output : Array.isArray(item.output) && object(item.output[0])?.type === 'input_text' ? object(item.output[0])?.text : undefined;
      if (typeof first !== 'string') throw new CodexRolloutError('missing_header');
      const boundary = first.indexOf('\nOutput:\n');
      if (boundary < 0 || boundary > 4096 || !/^Script (?:running with cell ID [^\s]{1,256}|completed|failed|terminated)\n/.test(first)) throw new CodexRolloutError('invalid_header');
      projected.output = first.slice(0, boundary + '\nOutput:\n'.length);
      this.calls.delete(callKey);
    }
    await this.options.onItem({ threadId: this.options.threadId, turnId: turn, item: projected });
  }
}
