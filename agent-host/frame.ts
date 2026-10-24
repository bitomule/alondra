import { z } from 'zod/v4';
import { createHash } from 'node:crypto';
import { types } from 'node:util';

export const PROTOCOL_VERSION = 1;
export const SDK_VERSION = '0.3.300';
export const MAX_FRAME_BYTES = 1_048_576;
const MAX_JSON_DEPTH = 127;
const text = (max = 256) => z.string().refine((s) => s.length > 0 && Buffer.byteLength(s) <= max && !s.includes('\0'));
const inputId = z.string().regex(/^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i);
const positive = z.number().int().min(1).max(Number.MAX_SAFE_INTEGER);
const digest = z.string().regex(/^[a-f0-9]{64}$/);
const object = z.record(z.string(), z.json());
const fence = z.strictObject({ profile_id: text(), account_id: text(), data_epoch: text(), host_id: text() });
/** The `lane` an orchestrator batch's execution names on the wire. */
export const ORCHESTRATOR_LANE = 'orchestrator';
const execution = z.union([
  z.strictObject({ task_id: text(), agent_id: text(), conversation_id: text(), process_instance_id: text(), generation: positive }),
  z.strictObject({ lane: z.literal(ORCHESTRATOR_LANE), run_id: text(), conversation_id: text(), generation: positive }),
]);
export type Execution = z.infer<typeof execution>;
/** The execution as the owner proofs bind it, in order; the generation comes last. */
export function executionSegments(value: Execution): string[] {
  return 'task_id' in value
    ? [value.task_id, value.agent_id, value.conversation_id, value.process_instance_id, String(value.generation)]
    : [ORCHESTRATOR_LANE, value.run_id, value.conversation_id, String(value.generation)];
}
export const MAX_MODEL_BYTES = 128;
export const MAX_MCP_SERVERS = 64;
/** The only permission modes Alondra can ask for: none of them grants a permission by itself. */
export const PERMISSION_MODES = ['default', 'plan', 'dont_ask'] as const;
export const DECISION_KINDS = ['permission', 'question', 'plan', 'elicitation'] as const;
export type DecisionKind = typeof DECISION_KINDS[number];
const decisionKind = z.enum(DECISION_KINDS);
const decision = z.discriminatedUnion('behavior', [
  z.strictObject({ behavior: z.literal('allow'), updated_input: object }), z.strictObject({ behavior: z.literal('deny'), message: text(MAX_FRAME_BYTES) }),
  z.strictObject({ behavior: z.literal('accept'), content: object.optional() }), z.strictObject({ behavior: z.literal('decline') }),
]);
export type Decision = z.infer<typeof decision>;
/** An elicitation comes from an MCP server, not from a tool call; every other request names its tool use. */
export const hasToolUse = (kind: DecisionKind): boolean => kind !== 'elicitation';
const answers = (kind: DecisionKind, behavior: Decision['behavior']): boolean => hasToolUse(kind) === (behavior === 'allow' || behavior === 'deny');
const action = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('start'), cwd: text(4096).refine((s) => s.startsWith('/')), input_id: inputId, prompt: text(MAX_FRAME_BYTES) }),
  z.strictObject({ type: z.literal('offer'), input_id: inputId, prompt: text(MAX_FRAME_BYTES) }),
  z.strictObject({ type: z.literal('interrupt') }), z.strictObject({ type: z.literal('force_stop') }), z.strictObject({ type: z.literal('close') }),
  z.strictObject({ type: z.literal('respond'), request_id: text(), epoch: positive, kind: decisionKind, tool_use_id: text().nullable(), decision, asked: object })
    .refine((respond) => hasToolUse(respond.kind) === (respond.tool_use_id !== null) && answers(respond.kind, respond.decision.behavior)),
  z.strictObject({ type: z.literal('set_model'), model: text(MAX_MODEL_BYTES).refine((s) => !/\p{Cc}/u.test(s) && s.trim().length > 0) }),
  z.strictObject({ type: z.literal('set_permission_mode'), mode: z.enum(PERMISSION_MODES) }),
  z.strictObject({ type: z.literal('mcp_status') }),
  z.strictObject({ type: z.literal('mcp_reconnect'), server: text() }),
  z.strictObject({ type: z.literal('mcp_authenticate'), server: text() }),
  z.strictObject({ type: z.literal('mcp_clear_auth'), server: text() }),
]);
export type Action = z.infer<typeof action>;
export const HOST_CAPABILITIES = ['streaming_input', 'exact_decisions', 'owner_attach', 'commit_ack', 'interrupt', 'force_stop', 'set_model', 'set_permission_mode', 'mcp_status', 'mcp_reconnect', 'mcp_sign_in', 'mcp_clear_auth'] as const;
export type Capability = typeof HOST_CAPABILITIES[number];
/** The capability that carries each action, as the daemon's `Action::capability` names it: an action added after the protocol's version names its own, so an owner never sends it to a host that cannot read it. */
export const ACTION_CAPABILITIES: Record<Action['type'], Capability | undefined> = {
  start: 'streaming_input', offer: 'streaming_input', respond: 'exact_decisions', interrupt: 'interrupt', force_stop: 'force_stop', close: undefined,
  set_model: 'set_model', set_permission_mode: 'set_permission_mode', mcp_status: 'mcp_status',
  mcp_reconnect: 'mcp_reconnect', mcp_authenticate: 'mcp_sign_in', mcp_clear_auth: 'mcp_clear_auth',
};
export const MAX_MCP_URL_BYTES = 2048;
export const MAX_SIGN_IN_URL_BYTES = 8192;
const mcpServers = z.array(z.strictObject({
  name: text(), status: text(), error: text().optional(), source: text().optional(), transport: text().optional(),
  url: text(MAX_MCP_URL_BYTES).refine((s) => !/[?#@]/.test(s)).optional(),
})).max(MAX_MCP_SERVERS);
export type McpServer = z.infer<typeof mcpServers>[number];
const signIn = z.discriminatedUnion('outcome', [
  z.strictObject({ outcome: z.literal('browser'), url: text(MAX_SIGN_IN_URL_BYTES).refine((s) => /^(https:\/\/|http:\/\/localhost|http:\/\/127\.0\.0\.1)/.test(s)), callback_port: z.number().int().min(1).max(65535).optional(), state: text().optional() })
    .refine((browser) => (browser.callback_port === undefined) === (browser.state === undefined)),
  z.strictObject({ outcome: z.literal('connected') }),
  z.strictObject({ outcome: z.literal('refused'), error: text() }),
]);
export type SignIn = z.infer<typeof signIn>;
const controlResult = z.discriminatedUnion('control', [
  z.strictObject({ control: z.literal('mcp_status'), command_id: text(), servers: mcpServers }),
  z.strictObject({ control: z.literal('mcp_reconnect'), command_id: text(), server: text(), error: text().optional(), servers: mcpServers }),
  z.strictObject({ control: z.literal('mcp_authenticate'), command_id: text(), server: text(), sign_in: signIn }),
  z.strictObject({ control: z.literal('mcp_clear_auth'), command_id: text(), server: text(), error: text().optional(), servers: mcpServers }),
]);
export type ControlResult = z.infer<typeof controlResult>;
const native = z.strictObject({ message_id: text().optional(), turn_id: text().optional(), tool_use_id: text().optional(), parent_tool_use_id: text().optional(), task_id: text().optional(), run_id: text().optional() });
const observation = z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('sdk'), kind: z.enum(['initialized', 'message', 'result', 'task', 'status', 'diagnostic', 'control_result', 'unsupported']), native, payload: object, failures: z.array(z.strictObject({ surface: z.enum(['assistant', 'startup', 'result', 'terminal', 'exit', 'hook', 'rate_limit', 'settings', 'mirror', 'auth', 'control', 'iterator', 'transport']), code: text(), details: z.json() })).max(32) })
    .refine((sdk) => sdk.kind !== 'control_result' || controlResult.safeParse(sdk.payload).success),
  z.strictObject({ type: z.literal('decision_requested'), request_id: text(), tool_use_id: text().optional(), kind: decisionKind, payload: object })
    .refine((requested) => hasToolUse(requested.kind) === (requested.tool_use_id !== undefined)),
  z.strictObject({ type: z.literal('decision_settled'), request_id: text(), epoch: positive, outcome: z.enum(['answered', 'cancelled']) }),
  z.strictObject({ type: z.literal('process_exited'), exit_code: z.number().int().min(-2147483648).max(2147483647).nullable(), signal: text(64).nullable() }),
]);
const ownerIdentity = { fence, execution, host_instance_id: text() };
const ownerGeneration = { owner_generation: positive };
const artifactIdentity = { host_release: text(), host_sha256: digest, executable_sha256: digest, sdk_version: text(64) };
const frameSchema = z.strictObject({ protocol_version: z.literal(PROTOCOL_VERSION), body: z.discriminatedUnion('type', [
  z.strictObject({ type: z.literal('challenge'), ...ownerIdentity, nonce: digest, executable_sha256: digest }),
  z.strictObject({ type: z.literal('attach'), ...ownerIdentity, ...ownerGeneration, ...artifactIdentity, nonce: digest, proof: digest }),
  z.strictObject({ type: z.literal('attached'), ...ownerIdentity, ...ownerGeneration, proof: digest }),
  z.strictObject({ type: z.literal('acknowledge'), ...ownerIdentity, ...ownerGeneration, cursor: positive }),
  z.strictObject({ type: z.literal('hello'), protocol_versions: z.array(z.number().int().min(1).max(65535)).min(1).max(16), capabilities: z.array(text(64)).max(32), fence, host_release: text(), host_sha256: digest, sdk_version: text(64) }),
  z.strictObject({ type: z.literal('command'), fence, execution, command_id: text(), deadline_ms: positive, action }),
  z.strictObject({ type: z.literal('receipt'), fence, execution, command_id: text(), command_sha256: digest, outcome: z.discriminatedUnion('status', [z.strictObject({ status: z.literal('accepted'), phase: z.enum(['admitted', 'offered', 'applied']) }), z.strictObject({ status: z.literal('rejected'), code: text(), message: text(MAX_FRAME_BYTES) })]) }),
  z.strictObject({ type: z.literal('event'), fence, execution, cursor: positive, observation }),
]) });
export type Frame = z.infer<typeof frameSchema>;

/** Check plain JSON before schema parsing or serialization can clone/allocate its contents. */
function boundedJSON(value: unknown): void {
  let bytes = 0;
  const active = new WeakSet<object>();
  const add = (amount: number) => { bytes += amount; if (bytes > MAX_FRAME_BYTES) throw new Error('Frame size'); };
  const string = (value: string) => {
    if (value.length > MAX_FRAME_BYTES) throw new Error('Frame size');
    if (/[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/u.test(value)) throw new Error('Invalid JSON string');
    add(2 + Buffer.byteLength(value));
    for (let i = 0; i < value.length; i += 1) {
      const code = value.charCodeAt(i);
      if (code === 34 || code === 92 || [8, 9, 10, 12, 13].includes(code)) add(1);
      else if (code < 32) add(5);
    }
  };
  const visit = (value: unknown, depth: number): void => {
    if (depth > MAX_JSON_DEPTH) throw new Error('JSON depth');
    if (value === null) { add(4); return; }
    if (typeof value === 'string') { string(value); return; }
    if (typeof value === 'boolean') { add(value ? 4 : 5); return; }
    if (typeof value === 'number' && Number.isFinite(value)) { add(String(value).length); return; }
    if (depth >= MAX_JSON_DEPTH) throw new Error('JSON depth');
    if (!value || typeof value !== 'object' || types.isProxy(value)) throw new Error('Plain JSON required');
    const prototype = Object.getPrototypeOf(value);
    if (prototype !== null && prototype !== Object.prototype && prototype !== Array.prototype) throw new Error('Plain JSON required');
    if (active.has(value)) throw new Error('Plain acyclic JSON required');
    for (let owner: object | null = value; owner; owner = Object.getPrototypeOf(owner)) {
      const serializer = Object.getOwnPropertyDescriptor(owner, 'toJSON');
      if (!serializer) continue;
      if (!('value' in serializer) || typeof serializer.value === 'function') throw new Error('Plain acyclic JSON required');
      break;
    }
    active.add(value);
    add(2);
    if (Array.isArray(value)) {
      if (value.length > MAX_FRAME_BYTES) throw new Error('Frame size');
      for (let i = 0; i < value.length; i += 1) {
        const descriptor = Object.getOwnPropertyDescriptor(value, String(i));
        if (!descriptor || !('value' in descriptor)) throw new Error('Plain JSON array required');
        if (i > 0) add(1);
        visit(descriptor.value, depth + 1);
      }
    } else {
      let first = true;
      for (const key in value) {
        if (!Object.hasOwn(value, key)) continue;
        const descriptor = Object.getOwnPropertyDescriptor(value, key)!;
        if (!('value' in descriptor)) throw new Error('Plain JSON object required');
        if (!first) add(1);
        first = false;
        string(key); add(1); visit(descriptor.value, depth + 1);
      }
    }
    active.delete(value);
  };
  visit(value, 0);
}
type ObjectContext = { type: 'object'; path: string[]; keys: Set<string>; key?: string; expectKey: boolean };
type ArrayContext = { type: 'array'; path: string[]; index: number };
function validateJSONTokens(source: string): void {
  const stack: (ObjectContext | ArrayContext)[] = [];
  const valuePath = (): string[] => {
    const parent = stack.at(-1);
    if (!parent) return [];
    return [...parent.path, parent.type === 'object' ? parent.key! : String(parent.index)];
  };
  const complete = () => {
    const parent = stack.at(-1);
    if (parent?.type === 'array') parent.index += 1;
    else if (parent) parent.key = undefined;
  };
  const integerField = (path: string[]): boolean =>
    path.join('.') === 'protocol_version' ||
    path.join('.') === 'body.execution.generation' ||
    path.join('.') === 'body.deadline_ms' ||
    path.join('.') === 'body.cursor' ||
    path.join('.') === 'body.owner_generation' ||
    path.join('.') === 'body.observation.exit_code' ||
    path.join('.') === 'body.action.epoch' ||
    path.join('.') === 'body.observation.epoch' ||
    (path.length === 3 && path[0] === 'body' && path[1] === 'protocol_versions');
  // JSON.parse has already checked grammar; this scanner only retains key and numeric spelling.
  for (const match of source.matchAll(/"(?:\\.|[^"\\])*"|-?(?:0|[1-9][0-9]*)(?:\.[0-9]+)?(?:[eE][+-]?[0-9]+)?|true|false|null|[{}\[\],:]/g)) {
    const token = match[0];
    const parent = stack.at(-1);
    if ((token === '{' || token === '[') && stack.length >= MAX_JSON_DEPTH) throw new Error('JSON depth');
    if (token === '{') stack.push({ type: 'object', path: valuePath(), keys: new Set(), expectKey: true });
    else if (token === '[') stack.push({ type: 'array', path: valuePath(), index: 0 });
    else if (token === '}' || token === ']') { stack.pop(); complete(); }
    else if (token === ',') { if (parent?.type === 'object') parent.expectKey = true; }
    else if (token === ':') continue;
    else if (parent?.type === 'object' && parent.expectKey) {
      const key = JSON.parse(token) as string;
      if (parent.keys.has(key)) throw new Error('Duplicate JSON key');
      parent.keys.add(key);
      parent.key = key;
      parent.expectKey = false;
    } else {
      if (/^-?[0-9]/.test(token) && integerField(valuePath()) && !/^(?:0|-?[1-9][0-9]*)$/.test(token)) throw new Error('Integer token required');
      complete();
    }
  }
}
export function parsePayload(payload: Uint8Array): Frame {
  if (payload.length === 0 || payload.length > MAX_FRAME_BYTES) throw new Error('Frame size');
  const source = new TextDecoder('utf-8', { fatal: true }).decode(payload);
  const value: unknown = JSON.parse(source);
  validateJSONTokens(source);
  boundedJSON(value);
  frameSchema.parse(value);
  return value as Frame;
}
export function encode(frame: Frame): Buffer {
  boundedJSON(frame);
  const payload = Buffer.from(JSON.stringify(frame));
  parsePayload(payload);
  const prefix = Buffer.alloc(4);
  prefix.writeUInt32BE(payload.length);
  return Buffer.concat([prefix, payload]);
}
export function decode(bytes: Buffer): Frame {
  if (bytes.length < 4) throw new Error('Incomplete frame header');
  const size = bytes.readUInt32BE();
  if (size === 0 || size > MAX_FRAME_BYTES || bytes.length !== size + 4) throw new Error('Incomplete or oversized frame');
  return parsePayload(bytes.subarray(4));
}
/** Identity is the original payload bytes, including their key order and whitespace. */
export function commandDigest(payload: Uint8Array): string {
  if (parsePayload(payload).body.type !== 'command') throw new Error('Command required');
  return createHash('sha256').update(payload).digest('hex');
}

/** Why this frame cannot be encoded as it is: its size, its depth or a lone UTF-16 surrogate. */
export function framingProblem(frame: unknown): 'size' | 'depth' | 'lone_surrogate' | 'not_plain_json' | undefined {
  try { boundedJSON(frame); return undefined; } catch (error) {
    const message = (error as Error).message;
    return message === 'Frame size' ? 'size' : message === 'JSON depth' ? 'depth' : message === 'Invalid JSON string' ? 'lone_surrogate' : 'not_plain_json';
  }
}
/** Twice the request has to fit an answer under the frame limit: in `asked` and in `updated_input`. */
export const MAX_DECISION_BYTES = 458_752;
export const MAX_DIAGNOSTIC_BYTES = 16_384;
/** The JSON's UTF-8 size and SHA-256, or nulls when it cannot even be serialized. */
export function jsonEvidence(value: unknown): { bytes: number | null; sha256: string | null } {
  try {
    const json = Buffer.from(JSON.stringify(value) ?? 'null');
    return { bytes: json.length, sha256: createHash('sha256').update(json).digest('hex') };
  } catch { return { bytes: null, sha256: null }; }
}
type Limits = { text: number; items: number; depth: number };
const STEPS: Limits[] = [
  { text: 65_536, items: 1024, depth: 64 }, { text: 4096, items: 64, depth: 32 },
  { text: 256, items: 8, depth: 12 }, { text: 0, items: 0, depth: 4 },
];
const WHOLE: Limits = { text: Infinity, items: Infinity, depth: 64 };
const LONE_SURROGATE = /[\uD800-\uDBFF](?![\uDC00-\uDFFF])|(?<![\uD800-\uDBFF])[\uDC00-\uDFFF]/gu;
const clip = (value: string, max: number) => (value.length > max ? value.slice(0, max) : value).replace(LONE_SURROGATE, '�');
const isRecord = (value: unknown): value is Record<string, unknown> => value !== null && typeof value === 'object' && !Array.isArray(value);
function shrink(value: unknown, limits: Limits, depth = 0): unknown {
  if (typeof value === 'string') return clip(value, limits.text);
  if (value === null || typeof value !== 'object') return value;
  if (depth >= limits.depth) return null;
  if (Array.isArray(value)) return value.slice(0, limits.items).map((item) => shrink(item, limits, depth + 1));
  return Object.fromEntries(Object.entries(value).slice(0, limits.items).map(([key, item]) => [clip(key, Infinity), shrink(item, limits, depth + 1)]));
}
/** Every field of an object, the ones `kept` whole; with no limits, only those. */
function select(value: Record<string, unknown>, limits: Limits | undefined, kept: (key: string, item: unknown) => unknown | undefined): Record<string, unknown> {
  const selected: Record<string, unknown> = {};
  for (const [key, item] of Object.entries(value)) {
    const whole = kept(key, item);
    if (whole !== undefined) selected[key] = whole;
    else if (limits) selected[clip(key, Infinity)] = shrink(item, limits, 1);
  }
  return selected;
}
const NORMALIZED = new Set(['type', 'subtype', 'uuid', 'session_id', 'parent_tool_use_id', 'user_message_uuid', 'user_message_uuids', 'task_id', 'run_id', 'status', 'error', 'origin', 'command_uuid']);
const NORMALIZED_RESULT = new Set(['worktreePath', 'worktreeBranch', 'agentId', 'status', 'action']);
const NORMALIZED_BLOCK = new Set(['type', 'name', 'id', 'tool_use_id']);
/** What the daemon's metrics read of a message: the model call it belongs to. */
const METERED_MESSAGE = new Set(['id', 'model', 'usage']);
/** An SDK message whose fields the normalizer and the metrics read stay whole; the rest is shrunk to `limits`. */
function shrinkMessage(payload: unknown, limits: Limits | undefined): unknown {
  if (!isRecord(payload)) return limits ? shrink(payload, limits) : null;
  const block = (item: unknown) => !isRecord(item) ? (limits ? shrink(item, limits, 3) : null) : select(item, limits, (key, field) =>
    NORMALIZED_BLOCK.has(key) ? shrink(field, WHOLE)
      : key === 'input' && isRecord(field) ? select(field, limits, (inner, value) => inner === 'isolation' ? shrink(value, WHOLE) : undefined) : undefined);
  return select(payload, limits, (key, item) => {
    if (NORMALIZED.has(key)) return shrink(item, WHOLE);
    if (key === 'tool_use_result' && isRecord(item)) return select(item, limits, (field, value) => NORMALIZED_RESULT.has(field) ? shrink(value, WHOLE) : undefined);
    if (key === 'message' && isRecord(item)) return select(item, limits, (field, value) => field === 'content' && Array.isArray(value) ? value.map(block)
      : METERED_MESSAGE.has(field) ? shrink(value, WHOLE) : undefined);
    return undefined;
  });
}
type Failure = { surface: string; code: string; details: unknown };
type SdkObservation = { type: 'sdk'; payload: unknown; failures: Failure[] };
/**
 * The observation as it can be framed: an SDK message too large, too deep or with a lone surrogate
 * is shrunk in ever stricter steps, never in the fields the normalizer reads, and says so in its
 * failures. `problem` names what keeps a candidate from being framed.
 */
export function fitObservation<T extends SdkObservation>(observation: T, problem: (candidate: T) => string | undefined): T {
  const reason = problem(observation);
  if (reason === undefined) return observation;
  const evidence: Failure = { surface: 'transport', code: 'truncated_sdk_message', details: { ...jsonEvidence(observation.payload), reason } };
  const failures = observation.failures.slice(0, 31);
  for (const limits of [...STEPS, undefined]) {
    const candidate = {
      ...observation,
      payload: shrinkMessage(observation.payload, limits),
      failures: [...failures.map((failure) => ({ ...failure, details: limits ? shrink(failure.details, limits) : null })), evidence],
    };
    if (problem(candidate) === undefined) return candidate;
  }
  throw new Error('Frame size');
}
/** The value shrunk until its JSON takes at most `budget` bytes; null when nothing of it fits. */
export function fitValue(value: unknown, budget: number): unknown {
  const fits = (candidate: unknown) => { const { bytes } = jsonEvidence(candidate); return bytes !== null && bytes <= budget && framingProblem(candidate) === undefined; };
  if (fits(value)) return value;
  for (const limits of STEPS) {
    const candidate = shrink(value, limits);
    if (fits(candidate)) return candidate;
  }
  return null;
}
