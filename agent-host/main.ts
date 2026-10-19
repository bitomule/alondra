import { Session } from './session.js';

// This entrypoint is a laboratory packaging probe, never wired to the daemon. It persists nothing, so every decision is denied.
const [executable, cwd, scenario] = process.argv.slice(2);
if (!executable || !cwd || !scenario || process.env.HOME !== cwd) throw new Error('Probe requires explicit executable, fixture cwd, scenario and fixture HOME');
let session: Session;
session = new Session({
  executable, cwd,
  env: { HOME: cwd, PATH: '/alondra/fixture/no-js-runtime', FIXTURE_SCENARIO: scenario, FIXTURE_CAPTURE: `${cwd}/capture.json`, FIXTURE_DECISION: `${cwd}/decision.json` },
  observe(observation) {
    process.stdout.write(JSON.stringify(observation) + '\n');
    if (observation.type === 'sdk' && observation.failures.some((failure) => failure.code !== 'aborted')) process.exitCode = 1;
    if (observation.type === 'sdk' && observation.kind === 'result') session.close();
  },
});
session.offer('00000000-0000-4000-8000-000000000002', 'Offline packaging probe');
session.finished.catch((error) => { process.stderr.write(String(error)); process.exitCode = 1; });
