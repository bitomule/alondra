import assert from 'node:assert/strict';
import { fstatSync, lstatSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { loadBootstrap, releaseBootstrap } from './bootstrap.js';
import { PersistentHost } from './persistent.js';

const config = loadBootstrap(fileURLToPath(import.meta.url));
let stopping;
const host = new PersistentHost(config, {
  observation(observation) {
    if (observation.type === 'sdk' && observation.kind === 'status') {
      const query = host.session.query;
      const close = query.close.bind(query);
      query.close = () => { close(); throw new Error('Controlled actual Query.close failure'); };
    }
    if (observation.type === 'process_exited') {
      setImmediate(() => {
        const first = host.stop();
        assert.equal(host.stop(), first, 'concurrent stop calls share exactly one cleanup promise');
        stopping = first.then(() => assert.fail('cleanup error must remain visible'), (error) => {
          assert.equal(host.server.listening, false, 'cleanup rejection must not leave the Unix server referenced');
          assert.throws(() => fstatSync(host.journal.fd), { code: 'EBADF' });
          assert.throws(() => fstatSync(0), { code: 'EBADF' });
          assert.equal(observation.exit_code, 7);
          assert(error instanceof AggregateError);
          assert(error.errors.length >= 1);
          process.stderr.write('Fixture resources released after actual native exit\n');
          process.exitCode = 23;
        });
        stopping.catch(async (error) => {
          console.error(error);
          // The native exit observation above permits independent fixture teardown on RED.
          for (const lane of host.lanes) lane.channel.socket.destroy();
          await new Promise((resolve) => host.server.close(resolve));
          try { host.journal.close(); } catch {}
          try { releaseBootstrap(config); } catch {}
          process.exitCode = 1;
        });
      });
    }
  },
});
process.on('SIGTERM', () => {
  // Structured test teardown still waits for real native exit.
  void host.stop().catch(() => { process.exitCode = 1; });
});
host.server.prependOnceListener('listening', () => {
  const bound = lstatSync(config.socket_path);
  process.stderr.write(`Fixture socket before normalization mode=${(bound.mode & 0o777).toString(8)}\n`);
});
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
