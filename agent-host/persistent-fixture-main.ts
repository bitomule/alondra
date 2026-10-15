import { loadBootstrap } from './bootstrap.js';
import { boundDiagnostics, listeningHint } from './diagnostics.js';
import { PersistentHost } from './persistent.js';

boundDiagnostics();
async function main(): Promise<void> {
  const config = loadBootstrap(__filename);
  const host = new PersistentHost(config, {
    observation() {
      console.log('Fixture console output outside framed channel');
      process.stdout.write('Fixture direct stdout outside framed channel\n');
      process.emitWarning('Fixture late diagnostic warning');
    },
  });
  const noise = process.env.FIXTURE_BOOTSTRAP_STDOUT;
  if (noise === 'noise' || noise === 'many-hints') {
    const write = (text: string) => new Promise<void>((resolve) => { process.stdout.write(text, () => resolve()); });
    await write(`${'x'.repeat(5000)}Alondra host: socket listening\n`);
    for (let count = 0; count < (noise === 'many-hints' ? 9 : 1); count++) await write('Alondra host: socket listening\n');
  }
  try { await host.listen(); } catch (error) { await host.stop(); throw error; }
  let stopping = false;
  const stop = () => { if (!stopping) { stopping = true; void host.stop().catch(() => { process.exitCode = 1; }); } };
  process.on('SIGTERM', stop); process.on('SIGINT', stop);
  await listeningHint();
}
main().catch(() => { process.stderr.write('Alondra fixture host startup refused\n'); process.exitCode = 1; });
