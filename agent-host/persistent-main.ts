import { fstatSync } from 'node:fs';
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
main().catch(() => { process.stderr.write('Alondra host startup refused\n'); process.exitCode = 1; });
