import assert from 'node:assert/strict';
import { fstatSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { CodexSession, CODEX_CAPABILITIES, CODEX_VERSION } from './codex-session.js';
import { PersistentHost } from './persistent-core.js';

const config = loadBootstrap(fileURLToPath(import.meta.url), CODEX_VERSION);
const host = new PersistentHost(config, (configuration) => new CodexSession(configuration), {
  capabilities: CODEX_CAPABILITIES, endsWithFailedSession: true,
  observation(observation) {
    // Hold the actual socket writer, as a non-reading owner does under backpressure.
    if (observation.type === 'process_exited') host.owner.channel.socket.cork();
  },
  endHostGroup() {
    assert(fstatSync(0).isFile(), 'cleanup must retain ownership until the old group ends');
    assert.throws(() => fstatSync(host.journal.fd), { code: 'EBADF' });
    process.stderr.write('Ownership retained through failed cleanup\n', () => process.kill(0, 'SIGKILL'));
  },
});
const close = host.journal.close.bind(host.journal);
host.journal.close = () => { close(); throw new Error('Controlled journal cleanup failure'); };
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
