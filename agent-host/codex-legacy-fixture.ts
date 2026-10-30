import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent-core.js';
import { CodexSession, CODEX_CAPABILITIES, CODEX_VERSION } from './codex-session.js';
import { boundDiagnostics, listeningHint } from './diagnostics.js';

// Reproduce an older host that keeps accepting owners after its native session has died.
async function main(): Promise<void> {
  const config = loadBootstrap(__filename, CODEX_VERSION);
  const host = new PersistentHost(config, (configuration) => new CodexSession(configuration), { capabilities: CODEX_CAPABILITIES });
  process.on('SIGTERM', () => { void host.stop(); });
  await host.listen();
  await listeningHint();
}
boundDiagnostics();
void main().catch(() => { process.exitCode = 1; });
