import assert from 'node:assert/strict';
import { join } from 'node:path';
import { ownedHosts } from './owned-host.mjs';

const { root, startHost, own, cleanup, stderr } = ownedHosts('ci-');
const offered = { status: 'accepted', phase: 'offered' };
const input = (n) => `00000000-0000-4000-8000-0000000000${String(n).padStart(2, '0')}`;

try {
  {
    const host = startHost('consecutive');
    const owner = await own(host);
    assert.deepEqual((await owner.command({ type: 'start', cwd: host.config.native.cwd, input_id: input(1), prompt: 'first prompt' })).outcome, offered, host.stderr());
    assert.deepEqual((await owner.command({ type: 'offer', input_id: input(2), prompt: 'second prompt' })).outcome, offered, host.stderr());
    assert.deepEqual((await owner.command({ type: 'offer', input_id: input(3), prompt: 'third prompt' })).outcome, offered, host.stderr());
  }
  {
    // 2026-11-03, t-deps-done-still-waiting: Claude could not start in a folder that no longer existed.
    // Its session ended before the next input arrived, and the host dropped the owner's connection
    // instead of saying the input was not offered, so every new host met the same fate.
    const folder = join(root, 'gone');
    const host = startHost('ended-session', { cwd: folder });
    const owner = await own(host);
    assert.deepEqual((await owner.command({ type: 'start', cwd: host.config.native.cwd, input_id: input(1), prompt: 'first prompt' })).outcome, offered, host.stderr());
    await owner.sdk('diagnostic', (payload) => Array.isArray(payload.unresolved_input_ids));
    const refused = await owner.command({ type: 'offer', input_id: input(2), prompt: 'second prompt' });
    assert.deepEqual(refused.outcome, { status: 'rejected', code: 'native_session_ended', message: 'The native session refused this input before submission' }, host.stderr());
  }
} catch (error) {
  console.error(stderr());
  throw error;
} finally {
  await cleanup();
}
console.log('ok');
