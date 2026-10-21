import { query } from '@anthropic-ai/claude-agent-sdk/core';
import { readFileSync } from 'node:fs';
import { isAbsolute } from 'node:path';
import { z } from 'zod/v4';
import type { McpServer } from './frame.js';
import { listed } from './persistent.js';

/** Started with this flag the host owns no execution: it lists a project's MCP servers and ends. */
export const PROBE_FLAG = '--mcp-probe';
const absolute = z.string().refine((value) => isAbsolute(value) && !value.includes('\0'));
const shape = z.strictObject({ executable: absolute, cwd: absolute, env: z.record(z.string(), z.string()) });
export type ProbeConfig = z.infer<typeof shape>;
/** How long servers that are still connecting may take before Claude's answer is taken as it is. */
const SETTLE_MS = 15_000;
const LOOK_MS = 500;

/** No message ever reaches Claude: the input stays open and empty until the probe closes it. */
async function* silent(): AsyncGenerator<never> {
  await new Promise<never>(() => {});
}

/** The MCP servers Claude has in `cwd`, as it lists them once those still connecting settle. */
export async function probe(config: ProbeConfig): Promise<McpServer[]> {
  const controller = new AbortController();
  const session = query({
    prompt: silent(),
    options: {
      cwd: config.cwd, pathToClaudeCodeExecutable: config.executable, env: config.env, abortController: controller,
      settingSources: ['user', 'project', 'local'], persistSession: false, permissionMode: 'default',
    },
  });
  try {
    const deadline = Date.now() + SETTLE_MS;
    for (;;) {
      const servers = listed(await session.mcpServerStatus());
      if (!servers.some((server) => server.status === 'pending') || Date.now() >= deadline) return servers;
      await new Promise((resolve) => setTimeout(resolve, LOOK_MS));
    }
  } finally {
    session.close();
    controller.abort();
  }
}

const written = (line: string) => new Promise<void>((resolve) => process.stdout.write(line + '\n', () => resolve()));

/** Reads its configuration from stdin and writes one JSON line: the servers, or why it could not. */
export async function runProbe(): Promise<void> {
  try {
    const config = shape.parse(JSON.parse(readFileSync(0, 'utf8')));
    await written(JSON.stringify({ servers: await probe(config) }));
  } catch (error) {
    const message = String((error as { message?: unknown })?.message ?? error).slice(0, 240);
    process.exitCode = 1;
    await written(JSON.stringify({ error: message }));
  }
}
