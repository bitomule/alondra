import type { OrchestratorProfile, WorkerProfile } from './bootstrap.js';
import type { Decision, DecisionKind } from './frame.js';

export type Failure = { surface: string; code: string; details: unknown };
export type Observation =
  | { type: 'sdk'; kind: string; native: Record<string, string>; payload: unknown; failures: Failure[] }
  | { type: 'decision_requested'; request_id: string; tool_use_id?: string; kind: DecisionKind; payload: unknown }
  | { type: 'decision_settled'; request_id: string; epoch: number; outcome: 'answered' | 'cancelled' }
  | { type: 'process_exited'; exit_code: number | null; signal: string | null };
export type DecisionRequest = { request_id: string; epoch: number; kind: DecisionKind; tool_use_id: string | null; asked: Record<string, unknown> };
export type HostConfiguration = {
  executable: string;
  cwd: string;
  env: Readonly<Record<string, string | undefined>>;
  observe: (observation: Observation) => number | void;
  beforeSpawn?: () => void;
  nativeRpcError?: (method: string, code: string, message: string) => void;
  sessionId?: string;
  resume?: boolean;
  stateDirectory?: string;
  processGeneration?: string;
  accountId?: string;
  hostRelease?: string;
  orchestrator?: OrchestratorProfile;
  worker?: WorkerProfile;
};
/** Positive adapter proof that this input was never submitted to the native model. */
export class InputNotOffered extends Error {
  constructor(readonly code: string) { super(code); }
}
/** Positive adapter proof that a native control made no state change. */
export class ControlNotApplied extends Error {
  constructor(readonly code: string) { super(code); }
}
export interface HostSession {
  readonly finished: Promise<void>;
  /** Set when the native session ended without being asked to: nothing it runs can go on. */
  readonly failure?: string;
  offer(inputId: string, prompt: string): void | Promise<void>;
  respond(request: DecisionRequest, decision: Decision): boolean;
  interrupt(): Promise<unknown>;
  interruptTurn(): Promise<void>;
  forceStop(): Promise<void>;
  close(): void;
  supportedModels(): Promise<ReadonlyArray<{ value: string }>>;
  setModel(model: string): Promise<void>;
  setPermissionMode(mode: 'default' | 'plan' | 'dontAsk'): Promise<void>;
  mcpServerStatus(): Promise<ReadonlyArray<{ name?: unknown; status?: unknown; error?: unknown; source?: unknown; config?: { type?: unknown; command?: unknown; url?: unknown } }>>;
  reconnectMcpServer(server: string): Promise<void>;
  mcpAuthenticate(server: string): Promise<unknown>;
  mcpClearAuth(server: string): Promise<unknown>;
}
export type SessionFactory = (configuration: HostConfiguration) => HostSession;
