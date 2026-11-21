import { existsSync, readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent.js';
import { PersistentHost as OwnedHost } from './persistent-core.js';
import { ControlNotApplied } from './host-session.js';
import { Session } from './session.js';

const config = loadBootstrap(fileURLToPath(import.meta.url));
const declared = process.env.FIXTURE_HOST_CAPABILITIES;
const hooks = declared === undefined ? {} : { capabilities: declared.split(',').filter(Boolean) };
const refusal = config.native.env.FIXTURE_CONTROL_REFUSAL;
// The Mac sleeps for as long as the test writes in this file: this process is frozen, so its
// monotonic clock stays where it was while its wall clock moves on by that much.
const slept = config.native.env.FIXTURE_SLEPT_FILE;
if (slept) {
  const wall = Date.now;
  Date.now = () => wall() + (existsSync(slept) ? Number(readFileSync(slept, 'utf8')) : 0);
}
const host = refusal ? new OwnedHost(config, (configuration) => {
  const session = new Session(configuration);
  session.setModel = async () => { throw refusal === 'typed' ? new ControlNotApplied('native_code_cells_active') : new Error('native_code_cells_active'); };
  return session;
}, hooks) : new PersistentHost(config, hooks);
process.on('SIGTERM', () => { void host.stop().catch(() => { process.exitCode = 1; }); });
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
