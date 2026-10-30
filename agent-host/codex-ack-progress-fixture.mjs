import assert from 'node:assert/strict';
import { createServer } from 'node:net';
import { join } from 'node:path';
import { createInterface } from 'node:readline';
import { mock } from 'node:test';
import { fileURLToPath } from 'node:url';
import { loadBootstrap } from './bootstrap.js';
import { CodexSession, CODEX_CAPABILITIES, CODEX_VERSION } from './codex-session.js';
import { PersistentHost } from './persistent-core.js';

const config = loadBootstrap(fileURLToPath(import.meta.url), CODEX_VERSION);
mock.timers.enable({ apis: ['setTimeout'] });
const host = new PersistentHost(config, (configuration) => new CodexSession(configuration), {
  capabilities: CODEX_CAPABILITIES, endsWithSession: true,
  endHostGroup: () => process.kill(0, 'SIGKILL'),
});
let renewals = 0;
let wrapped;
const pending = [];
function advanceReady() {
  for (let i = pending.length - 1; i >= 0; i--) {
    const request = pending[i];
    if (host.journal.acknowledged < request.afterAck) continue;
    pending.splice(i, 1);
    const ended = host.sessionEnd;
    assert(ended?.progress, 'the normal terminal session has an inactivity renewal');
    if (wrapped !== ended) {
      const renew = ended.progress;
      ended.progress = () => { renewals++; renew(); };
      wrapped = ended;
    }
    mock.timers.tick(request.tick);
    request.socket.write(JSON.stringify({ acknowledged: host.journal.acknowledged, renewals }) + '\n');
  }
}
const append = host.journal.append.bind(host.journal);
host.journal.append = (...args) => {
  const result = append(...args);
  if (args[0].body.type === 'acknowledge') queueMicrotask(advanceReady);
  return result;
};
const clock = createServer((socket) => {
  const input = createInterface({ input: socket });
  input.on('line', (line) => {
    const command = JSON.parse(line);
    assert(Number.isSafeInteger(command.tick) && command.tick >= 0);
    assert(Number.isSafeInteger(command.afterAck) && command.afterAck >= 0);
    pending.push({ ...command, socket });
    advanceReady();
  });
});
await new Promise((resolve) => clock.listen(join(config.state_dir, 'clock'), resolve));
await host.listen();
process.stdout.write('Alondra host: socket listening\n');
