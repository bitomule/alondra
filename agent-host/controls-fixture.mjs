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
const host = refusal ? new OwnedHost(config, (configuration) => {
  const session = new Session(configuration);
  session.setModel = async () => { throw refusal === 'typed' ? new ControlNotApplied('native_code_cells_active') : new Error('native_code_cells_active'); };
  return session;
}, hooks) : new PersistentHost(config, hooks);
process.on('SIGTERM', () => { void host.stop().catch(() => { process.exitCode = 1; }); });
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
