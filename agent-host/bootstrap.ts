import { z } from 'zod/v4';
import { createHash } from 'node:crypto';
import { chmodSync, closeSync, constants, fstatSync, lstatSync, openSync, readSync, realpathSync, unlinkSync } from 'node:fs';
import { isAbsolute, join } from 'node:path';
import { parsePayload, PROTOCOL_VERSION, SDK_VERSION, type Frame } from './frame.js';

const digest = z.string().regex(/^[a-f0-9]{64}$/);
const absolute = z.string().refine((value) => isAbsolute(value) && !value.includes('\0'));
const shape = z.strictObject({
  state_dir: absolute, ownership_path: absolute, socket_path: absolute,
  host_instance_id: z.string().min(1).max(256), control_secret: digest,
  fence: z.unknown(), execution: z.unknown(),
  manifest: z.strictObject({ mode: z.literal('node_bundle'), artifact_path: absolute, host_release: z.string().min(1).max(256), host_sha256: digest, executable_sha256: digest, sdk_version: z.literal(SDK_VERSION) }),
  native: z.strictObject({ session_id: z.string().regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/), executable: absolute, cwd: absolute, env: z.record(z.string(), z.string()) }),
  orchestrator: z.strictObject({
    resume: z.boolean(),
    model: z.string().min(1).max(64),
    tools: z.array(z.string().min(1).max(128)).max(16),
    disallowed_tools: z.array(z.string().min(1).max(128)).max(16),
    mcp_servers: z.record(z.string().regex(/^[a-z0-9-]{1,64}$/), z.strictObject({ command: absolute, args: z.array(z.string().max(256)).max(16) })),
    plugins: z.array(absolute).max(8),
    settings: z.record(z.string(), z.json()),
  }).optional(),
  worker: z.strictObject({
    plugins: z.array(absolute).max(8),
    settings: z.record(z.string(), z.json()),
  }).optional(),
}).refine((config) => !(config.orchestrator && config.worker), 'A host runs the orchestrator or a task, never both');
export type OrchestratorProfile = NonNullable<z.infer<typeof shape>['orchestrator']>;
export type WorkerProfile = NonNullable<z.infer<typeof shape>['worker']>;
const verified = Symbol('verified bootstrap');
const retained = new WeakMap<object, { fd: number; dev: number; ino: number }>();
type Identity = Extract<Frame['body'], { type: 'attached' }>;
export type Bootstrap = Omit<z.infer<typeof shape>, 'fence' | 'execution'> & Pick<Identity, 'fence' | 'execution'> & { readonly [verified]: true };

export function fileHash(path: string): string {
  const fd = openSync(path, constants.O_RDONLY);
  try {
    const hash = createHash('sha256'); const buffer = Buffer.alloc(65536);
    for (;;) { const count = readSync(fd, buffer, 0, buffer.length, null); if (!count) break; hash.update(buffer.subarray(0, count)); }
    return hash.digest('hex');
  } finally { closeSync(fd); }
}
function privateNode(path: string, kind: 'file' | 'directory' | 'socket'): void {
  const stat = lstatSync(path);
  if (stat.isSymbolicLink() || stat.uid !== process.getuid?.() || (stat.mode & 0o777) !== (kind === 'directory' ? 0o700 : 0o600)
      || !(kind === 'directory' ? stat.isDirectory() : kind === 'socket' ? stat.isSocket() : stat.isFile())) throw new Error('Private ownership path refused');
}
/** The private Rust launcher acquired the flock before transferring this same file as stdin. */
export function loadBootstrap(actualArtifact: string): Bootstrap {
  const fd = 0;
  const stat = fstatSync(fd);
  if (!stat.isFile() || stat.nlink !== 1 || stat.uid !== process.getuid?.() || (stat.mode & 0o777) !== 0o600 || stat.size < 2 || stat.size > 65536) throw new Error('Private bootstrap refused');
  const raw = Buffer.alloc(stat.size); let offset = 0;
  while (offset < raw.length) { const count = readSync(fd, raw, offset, raw.length - offset, offset); if (!count) throw new Error('Bootstrap truncated'); offset += count; }
  const after = fstatSync(fd);
  if (after.dev !== stat.dev || after.ino !== stat.ino || after.size !== stat.size || after.mtimeMs !== stat.mtimeMs) throw new Error('Bootstrap changed during read');
  const config = shape.parse(JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(raw)));
  const frame = parsePayload(Buffer.from(JSON.stringify({ protocol_version: PROTOCOL_VERSION, body: { type: 'challenge', fence: config.fence, execution: config.execution, host_instance_id: config.host_instance_id, nonce: '0'.repeat(64), executable_sha256: config.manifest.executable_sha256 } })));
  if (frame.body.type !== 'challenge') throw new Error('Bootstrap identity');
  if (config.ownership_path !== join(config.state_dir, 'host.lock') || config.socket_path !== join(config.state_dir, 's')) throw new Error('Ownership binding paths');
  privateNode(config.state_dir, 'directory'); privateNode(config.ownership_path, 'file');
  const node = lstatSync(config.ownership_path);
  if (node.dev !== stat.dev || node.ino !== stat.ino || realpathSync(config.state_dir) !== config.state_dir) throw new Error('Ownership file replaced');
  if (realpathSync(config.manifest.artifact_path) !== realpathSync(actualArtifact)
      || fileHash(actualArtifact) !== config.manifest.host_sha256 || fileHash(process.execPath) !== config.manifest.executable_sha256
      || process.version !== 'v24.19.0') throw new Error('Owned artifact manifest mismatch');
  if (Object.values(config.native.env).some((value) => value.includes(config.control_secret))) throw new Error('Control proof must not reach native environment');
  const result: Bootstrap = { ...config, fence: frame.body.fence, execution: frame.body.execution, [verified]: true };
  retained.set(result, { fd, dev: stat.dev, ino: stat.ino });
  return result;
}
export function assertBootstrap(config: Bootstrap): void {
  const ownership = retained.get(config);
  if (!ownership) throw new Error('Retained bootstrap absent');
  const descriptor = fstatSync(ownership.fd); const node = lstatSync(config.ownership_path);
  if (descriptor.dev !== ownership.dev || descriptor.ino !== ownership.ino || node.dev !== ownership.dev || node.ino !== ownership.ino) throw new Error('Retained bootstrap replaced');
  privateNode(config.state_dir, 'directory'); privateNode(config.ownership_path, 'file');
}
export function prepareSocket(config: Bootstrap): void {
  assertBootstrap(config);
  try { privateNode(config.socket_path, 'socket'); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === 'ENOENT') return; throw error; }
  // Only the privately transferred exclusive lock authorizes removing this stale owned endpoint.
  unlinkSync(config.socket_path);
}
export function secureSocket(config: Bootstrap): void {
  assertBootstrap(config);
  const before = lstatSync(config.socket_path);
  if (before.isSymbolicLink() || !before.isSocket() || before.uid !== process.getuid?.() || (before.mode & 0o077) !== 0) throw new Error('Private bound socket refused');
  chmodSync(config.socket_path, 0o600);
  checkSocket(config);
  const after = lstatSync(config.socket_path);
  if (after.dev !== before.dev || after.ino !== before.ino) throw new Error('Bound socket replaced');
}
export function checkSocket(config: Bootstrap): void { assertBootstrap(config); privateNode(config.socket_path, 'socket'); }
export function releaseBootstrap(config: Bootstrap): void {
  const ownership = retained.get(config);
  if (ownership) { retained.delete(config); closeSync(ownership.fd); }
}
