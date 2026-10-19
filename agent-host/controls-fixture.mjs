import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent.js';

const config = loadBootstrap(fileURLToPath(import.meta.url));
const declared = process.env.FIXTURE_HOST_CAPABILITIES;
const host = new PersistentHost(config, declared === undefined ? {} : { capabilities: declared.split(',').filter(Boolean) });
process.on('SIGTERM', () => { void host.stop().catch(() => { process.exitCode = 1; }); });
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
