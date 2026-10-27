import { fstatSync } from 'node:fs';
import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent-core.js';
import { CodexSession, CODEX_CAPABILITIES, CODEX_VERSION } from './codex-session.js';
import { boundDiagnostics, listeningHint } from './diagnostics.js';



async function main(): Promise<void> {
  const config = loadBootstrap(__filename, CODEX_VERSION);
  const host = new PersistentHost(config, (configuration) => new CodexSession(configuration), { capabilities: CODEX_CAPABILITIES });
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
boundDiagnostics();
main().catch(() => { process.stderr.write('Alondra Codex host startup refused\n'); process.exitCode = 1; });
