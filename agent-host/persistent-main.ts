import { fstatSync } from 'node:fs';
import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent.js';
import { boundDiagnostics, listeningHint } from './diagnostics.js';
import { PROBE_FLAG, runProbe } from './probe.js';
import { USAGE_PROBE_FLAG, probeUsage } from './usage-probe.js';

/** `--usage-probe`: reads the probe's request on stdin and writes what the account has used. */
async function probe(): Promise<void> {
  const chunks: Buffer[] = [];
  for await (const chunk of process.stdin) chunks.push(chunk as Buffer);
  const report = await probeUsage(JSON.parse(Buffer.concat(chunks).toString('utf8')));
  process.stdout.write(JSON.stringify(report) + '\n');
}

async function main(): Promise<void> {
  const config = loadBootstrap(__filename);
  const host = new PersistentHost(config);
  try { await host.listen(); } catch (error) { await host.stop(); throw error; }
  let stopping = false;
  const stop = (interruptTurn: boolean) => { if (!stopping) { stopping = true; void host.stop(interruptTurn).catch(() => { process.exitCode = 1; }); } };
  process.on('SIGTERM', () => stop(false)); process.on('SIGINT', () => stop(true));
  // With its ownership lock unlinked no owner can ever adopt this host: end the whole group.
  const lock = fstatSync(0);
  const unadoptable = setInterval(() => {
    let now;
    try { now = fstatSync(0); } catch { clearInterval(unadoptable); return; }
    if (now.dev !== lock.dev || now.ino !== lock.ino) { clearInterval(unadoptable); return; }
    if (now.nlink === 0 && !stopping) process.kill(0, 'SIGKILL');
  }, 1000);
  unadoptable.unref();
  await listeningHint();
}
if (process.argv.includes(PROBE_FLAG)) void runProbe().then(() => process.exit());
else if (process.argv.includes(USAGE_PROBE_FLAG)) {
  probe().then(() => process.exit(0), (error) => { process.stderr.write(`${error instanceof Error ? error.message : String(error)}\n`.slice(0, 512)); process.exit(1); });
} else {
  boundDiagnostics();
  main().catch(() => { process.stderr.write('Alondra host startup refused\n'); process.exitCode = 1; });
}
