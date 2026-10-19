import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent.js';

// A bare persistent host over a real socket with the fake native: no fault hooks, so the
// reconcile test drives attach, offer and re-send itself.
const config = loadBootstrap(fileURLToPath(import.meta.url));
const host = new PersistentHost(config);
process.on('SIGTERM', () => { void host.stop().catch(() => { process.exitCode = 1; }); });
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
