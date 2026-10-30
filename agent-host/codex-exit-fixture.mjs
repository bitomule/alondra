import { mkdirSync } from 'node:fs';
import { join } from 'node:path';
import { CodexSession } from './codex-session.js';

const [home, executable] = process.argv.slice(2);
const stateDirectory = join(home, 'execution');
mkdirSync(stateDirectory, { recursive: true, mode: 0o700 });
const session = new CodexSession({
  executable, cwd: home, stateDirectory,
  sessionId: '00000000-0000-4000-8000-000000000002',
  env: {
    HOME: home, PATH: '/usr/bin:/bin', ALONDRA_HOME: home,
    ALONDRA_DOMAIN_CAPABILITY: 'fixture-capability', ALONDRA_DOMAIN_PROTOCOL: '2',
    ALONDRA_SESSION_ID: 'fixture-node', ALONDRA_PROVIDER: 'codex',
    ALONDRA_FAKE_MODE: 'exit_with_descendant', ALONDRA_FAKE_DESCENDANT: join(home, 'descendant'),
  },
  observe() {},
});
session.offer('00000000-0000-4000-8000-000000000003', 'Exit after spawning a descendant');
await session.finished;
