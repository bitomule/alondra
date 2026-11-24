import assert from 'node:assert/strict';
import { fstatSync } from 'node:fs';
import { mock } from 'node:test';
import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { CodexSession, CODEX_CAPABILITIES, CODEX_VERSION } from './codex-session.js';
import { PersistentHost } from './persistent-core.js';

const config = loadBootstrap(fileURLToPath(import.meta.url), CODEX_VERSION);
// Every deadline passes only when this fixture ticks it: a slow machine never lets the backstop that
// ends the group fire before the cleanup it backs up.
mock.timers.enable({ apis: ['setTimeout'] });
const host = new PersistentHost(config, (configuration) => {
  const session = new CodexSession(configuration);
  // The native's descendant keeps its output open: the session lets go of it after its drain deadline.
  session.child.once('exit', () => setImmediate(() => mock.timers.tick(1000)));
  return session;
}, {
  capabilities: CODEX_CAPABILITIES, endsWithFailedSession: true,
  observation(observation) {
    // Hold the actual socket writer, as a non-reading owner does under backpressure.
    if (observation.type === 'process_exited') {
      host.owner.channel.socket.cork();
      // The host sets its failure deadlines once the session's end settles, which is after this
      // call and before the next macrotask. Only the first one passes: cleanup unblocks the output.
      setImmediate(() => mock.timers.tick(1000));
    }
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
