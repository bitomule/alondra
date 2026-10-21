import assert from 'node:assert/strict';
import { lstatSync } from 'node:fs';
import { join } from 'node:path';
import { ownedHosts } from './owned-host.mjs';

process.umask(0o022);
const { startHost, own, cleanup, stderr } = ownedHosts('um-');
const mode = (path) => lstatSync(path).mode & 0o777;

try {
  const host = startHost('node', { nativeEnv: { FIXTURE_CREATES: 'created' } });
  const owner = await own(host);
  const launch = await owner.command({ type: 'start', cwd: host.config.native.cwd, input_id: '00000000-0000-4000-8000-000000000001', prompt: 'create files' });
  assert.deepEqual(launch.outcome, { status: 'accepted', phase: 'offered' }, host.stderr());
  await owner.sdk('result');

  const created = join(host.config.state_dir, 'created');
  assert.equal(mode(created).toString(8), '755', 'a folder a command creates follows the user umask');
  assert.equal(mode(join(created, 'file')).toString(8), '644', 'a file a command creates follows the user umask');
  assert.equal(mode(host.config.socket_path).toString(8), '600', 'the host socket stays owner-only');
  assert.equal(mode(join(host.config.state_dir, 'outbox')).toString(8), '600', 'the host journal stays owner-only');
  assert.equal(mode(host.config.state_dir).toString(8), '700', 'the host state folder stays owner-only');
} catch (error) {
  console.error(stderr());
  throw error;
} finally {
  await cleanup();
}
