import { query, type SDKUserMessage, type SpawnOptions, type SpawnedProcess } from '@anthropic-ai/claude-agent-sdk/core';
import { isAbsolute } from 'node:path';
import { spawn as spawnNative } from 'node:child_process';
import { z } from 'zod/v4';

export const MODEL_PROBE_FLAG = '--model-probe';
const absolute = z.string().refine((value) => isAbsolute(value) && !value.includes('\0'));
const request = z.strictObject({ executable: absolute, cwd: absolute, env: z.record(z.string(), z.string()), timeout_ms: z.number().int().min(1_000).max(120_000) });

export type ModelProbeReport = {
  native_default: null;
  models: { model: string; display_name: string; reasoning_efforts: string[]; default_reasoning_effort: null; context_window: null; input_modalities: string[]; is_default: false; hidden: false }[];
};

async function* silence(stopped: Promise<void>): AsyncGenerator<SDKUserMessage> { await stopped; }

export async function probeModels(input: unknown, spawn?: (options: SpawnOptions) => SpawnedProcess): Promise<ModelProbeReport> {
  const { executable, cwd, env, timeout_ms } = request.parse(input);
  let stop!: () => void;
  const stopped = new Promise<void>((resolve) => { stop = resolve; });
  const controller = new AbortController();
  let child: SpawnedProcess | undefined;
  let closed: Promise<void> | undefined;
  const session = query({ prompt: silence(stopped), options: {
    cwd, env, pathToClaudeCodeExecutable: executable, abortController: controller,
    settingSources: [], strictMcpConfig: true, mcpServers: {}, tools: [], persistSession: false,
    spawnClaudeCodeProcess(options) {
      child = spawn ? spawn(options) : spawnNative(options.command, options.args, { cwd: options.cwd, env: options.env, signal: options.signal, stdio: ['pipe', 'pipe', 'pipe'] });
      closed = new Promise<void>((resolve) => { child!.on('exit', () => resolve()); child!.on('error', () => resolve()); });
      return child;
    },
  } });
  const timeout = setTimeout(() => controller.abort(), timeout_ms);
  try {
    await session.initializationResult();
    const models = await session.supportedModels();
    if (models.length > 1024) throw new Error('The model catalog exceeds its limit');
    return { native_default: null, models: models.map((model) => ({
      model: model.value, display_name: model.displayName, reasoning_efforts: model.supportedEffortLevels ?? [],
      default_reasoning_effort: null, context_window: null, input_modalities: [], is_default: false, hidden: false,
    })) };
  } finally {
    clearTimeout(timeout); stop(); session.close(); child?.kill('SIGKILL'); await closed;
  }
}
