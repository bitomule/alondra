import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent.js';
import { boundDiagnostics, listeningHint } from './diagnostics.js';
boundDiagnostics();

async function main(): Promise<void> {
  const config = loadBootstrap(__filename);
  const host = new PersistentHost(config);
  try { await host.listen(); } catch (error) { await host.stop(); throw error; }
  let stopping = false;
  const stop = () => { if (!stopping) { stopping = true; void host.stop().catch(() => { process.exitCode = 1; }); } };
  process.on('SIGTERM', stop); process.on('SIGINT', stop);
  await listeningHint();
}
main().catch(() => { process.stderr.write('Alondra host startup refused\n'); process.exitCode = 1; });
