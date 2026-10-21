import { query, type SDKUserMessage } from '@anthropic-ai/claude-agent-sdk';
import { isAbsolute } from 'node:path';
import { z } from 'zod/v4';

/** Started with this flag the host owns no execution: it reads one account's usage and ends. */
export const USAGE_PROBE_FLAG = '--usage-probe';
const absolute = z.string().refine((value) => isAbsolute(value) && !value.includes('\0'));
const request = z.strictObject({ executable: absolute, cwd: absolute, env: z.record(z.string(), z.string()), timeout_ms: z.number().int().min(1_000).max(120_000) });
export type ProbeRequest = z.infer<typeof request>;
type Window = { utilization: number | null; resets_at: string | null } | null | undefined;
export type ProbeReport = { plan?: string; rate_limits_available: boolean; windows: { kind: string; used: number; resets_at?: number }[] };

/** A prompt that never sends a message: the session starts, answers controls and costs no turn. */
async function* silence(stopped: Promise<void>): AsyncGenerator<SDKUserMessage> {
  await stopped;
}

/**
 * What an account has used of its plan, read without a prompt: Claude starts with the account's
 * configuration folder, says which account it signed in with as it initializes, and answers the
 * usage the `/usage` command shows. Nothing of the user's settings, plugins or MCP servers loads.
 */
export async function probeUsage(input: unknown): Promise<ProbeReport> {
  const { executable, cwd, env, timeout_ms } = request.parse(input);
  let stop!: () => void;
  const stopped = new Promise<void>((resolve) => { stop = resolve; });
  const controller = new AbortController();
  const session = query({
    prompt: silence(stopped),
    options: {
      cwd, env, pathToClaudeCodeExecutable: executable, abortController: controller,
      settingSources: [], strictMcpConfig: true, mcpServers: {}, tools: [], persistSession: false,
    },
  });
  const timer = setTimeout(() => controller.abort(), timeout_ms);
  try {
    const initialized = await session.initializationResult();
    const usage = await session.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET({ skipBehaviors: true });
    const plan = initialized.account?.subscriptionType ?? usage.subscription_type ?? undefined;
    const limits = (usage.rate_limits ?? {}) as Record<string, unknown>;
    const windows = Object.entries(limits).flatMap(([kind, value]) => {
      const window = value as Window;
      if (kind === 'model_scoped' || kind === 'extra_usage' || !window || typeof window !== 'object' || typeof window.utilization !== 'number') return [];
      const resets = typeof window.resets_at === 'string' ? Date.parse(window.resets_at) : Number.NaN;
      return [{ kind, used: window.utilization / 100, ...(Number.isFinite(resets) ? { resets_at: resets } : {}) }];
    });
    return { ...(typeof plan === 'string' ? { plan } : {}), rate_limits_available: usage.rate_limits_available, windows };
  } finally {
    clearTimeout(timer);
    stop();
    session.close();
  }
}
