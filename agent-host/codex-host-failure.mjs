import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createHash, randomBytes } from 'node:crypto';
import { once } from 'node:events';
import { closeSync, mkdtempSync, openSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs';
import { createConnection } from 'node:net';
import { tmpdir } from 'node:os';
import { join, resolve } from 'node:path';
import { createInterface } from 'node:readline';
import { Channel } from './channel.js';
import { CODEX_VERSION } from './codex-session.js';
import { parsePayload, PROTOCOL_VERSION } from './frame.js';
import { ownerProof } from './ownership.js';

export async function failedAdoptedHost(t, artifactName = 'codex.cjs', cleanupProof = false, normal = false, acknowledge = false, progress = false) {
  const root = mkdtempSync(join(realpathSync(tmpdir()), 'acf-'));
  const artifact = realpathSync(resolve('agent-host', artifactName));
  const executable = realpathSync(process.env.JS_BINARY__NODE_BINARY);
  const hash = (path) => createHash('sha256').update(readFileSync(path)).digest('hex');
  const fence = { profile_id: 'default', account_id: 'account-test', data_epoch: 'epoch-test', host_id: 'host-test' };
  const execution = { task_id: 'task-test', agent_id: 'agent-test', conversation_id: 'conversation-test', process_instance_id: 'process-test', generation: 7 };
  const descendantFile = join(root, 'descendant');
  const config = {
    state_dir: root, ownership_path: join(root, 'host.lock'), socket_path: join(root, 's'),
    host_instance_id: 'native-exit-fixture', control_secret: randomBytes(32).toString('hex'), fence, execution,
    manifest: { mode: 'node_bundle', artifact_path: artifact, host_release: 'native-exit-fixture', host_sha256: hash(artifact), executable_sha256: hash(executable), sdk_version: CODEX_VERSION },
    native: { session_id: '00000000-0000-4000-8000-000000000010', executable: realpathSync(resolve('agent-host/fake-codex.py')), cwd: root,
      env: { HOME: root, PATH: '/usr/bin:/bin', ALONDRA_HOME: root, ALONDRA_DOMAIN_CAPABILITY: 'fixture-capability', ALONDRA_DOMAIN_PROTOCOL: '2', ALONDRA_SESSION_ID: 'fixture-node', ALONDRA_PROVIDER: 'codex', ALONDRA_FAKE_MODE: 'idle_with_descendant', ...(progress ? { ALONDRA_FAKE_DESCENDANT_QUIET: '1' } : {}), ALONDRA_FAKE_DESCENDANT: descendantFile, ALONDRA_FAKE_ACCOUNT_FILE: join(root, 'account.json') } },
  };
  writeFileSync(config.ownership_path, JSON.stringify(config), { mode: 0o600 });
  writeFileSync(join(root, 'account.json'), JSON.stringify(normal ? {} : { exitMcp: true }));
  const descriptor = openSync(config.ownership_path, 'r');
  // The actual bundle runs in its own group, as OwnedHost launches it in production.
  const child = spawn(executable, [artifact], { cwd: root, env: { HOME: root, PATH: '/usr/bin:/bin' }, detached: true, stdio: [descriptor, 'pipe', 'pipe'] });
  closeSync(descriptor);
  const exited = once(child, 'exit');
  let stderr = ''; child.stderr.on('data', (bytes) => { stderr = (stderr + bytes).slice(-16384); });
  let socket; let descendant; let watcher; let watched; let clock;
  t.after(async () => {
    try {
      socket?.destroy(); clock?.destroy();
      if (child.exitCode === null && child.signalCode === null) process.kill(-child.pid, 'SIGKILL');
      await exited;
      if (!descendant) { try { descendant = JSON.parse(readFileSync(descendantFile, 'utf8')); } catch (error) { if (error.code !== 'ENOENT') throw error; } }
      if (descendant) { try { process.kill(descendant, 'SIGKILL'); } catch (error) { if (error.code !== 'ESRCH') throw error; } }
      if (watcher && watcher.exitCode === null && watcher.signalCode === null) watcher.kill('SIGKILL');
      if (watched) await watched;
    } finally { rmSync(root, { recursive: true, force: true }); }
  });
  const ready = createInterface({ input: child.stdout });
  const [hint] = await Promise.race([once(ready, 'line'), exited.then(() => { throw new Error(stderr); })]);
  assert.equal(hint, 'Alondra host: socket listening'); ready.close();
  async function own(generation) {
    socket = createConnection(config.socket_path);
    const frames = []; const waits = [];
    const channel = new Channel(socket, (frame) => {
      frames.push(frame);
      for (let i = waits.length - 1; i >= 0; i--) if (waits[i].predicate(frame)) waits.splice(i, 1)[0].resolve(frame);
    }, () => {});
    socket.once('close', () => { for (const waiter of waits.splice(0)) waiter.reject(new Error('Owner lane closed')); });
    const next = (predicate) => frames.find(predicate) ? Promise.resolve(frames.find(predicate)) : new Promise((resolve, reject) => waits.push({ predicate, resolve, reject }));
    const send = (body) => channel.write({ protocol_version: PROTOCOL_VERSION, body });
    const challenge = (await next((frame) => frame.body.type === 'challenge')).body;
    const attach = { type: 'attach', fence, execution, host_instance_id: config.host_instance_id, nonce: challenge.nonce, owner_generation: generation, host_release: config.manifest.host_release, host_sha256: config.manifest.host_sha256, executable_sha256: config.manifest.executable_sha256, sdk_version: CODEX_VERSION };
    attach.proof = ownerProof(Buffer.from(config.control_secret, 'hex'), attach);
    await send(attach); await next((frame) => frame.body.type === 'attached');
    return { send, next };
  }
  const first = await own(1);
  await first.send({ type: 'command', fence, execution, command_id: 'start-fixture', deadline_ms: Date.now() + 60000, action: { type: 'start', cwd: root, input_id: '00000000-0000-4000-8000-000000000002', prompt: 'Rest with a command still alive' } });
  await first.next((frame) => frame.body.type === 'event' && frame.body.observation.type === 'sdk' && frame.body.observation.payload.method === 'turn/completed');
  descendant = JSON.parse(readFileSync(descendantFile, 'utf8'));
  socket.destroy();
  const adopted = await own(2);
  // Subscribe before triggering exit; NOTE_EXIT detects zombies too and does not reap them.
  const script = 'import select,sys; q=select.kqueue(); pids=[int(p) for p in sys.argv[1:]]; q.control([select.kevent(p,filter=select.KQ_FILTER_PROC,flags=select.KQ_EV_ADD|select.KQ_EV_ONESHOT,fflags=select.KQ_NOTE_EXIT) for p in pids],0,None); print("ready",flush=True); pending=set(pids)\nwhile pending:\n for event in q.control(None,len(pending),None): pending.discard(event.ident)';
  watcher = spawn('/usr/bin/python3', ['-c', script, String(child.pid), String(descendant)], { stdio: ['ignore', 'pipe', 'inherit'] });
  watched = once(watcher, 'exit');
  const watching = createInterface({ input: watcher.stdout });
  assert.deepEqual(await Promise.race([once(watching, 'line'), watched.then(() => { throw new Error('Exit watcher ended before subscribing'); })]), ['ready']);
  watching.close();
  await adopted.send({ type: 'command', fence, execution, command_id: 'exit-fixture', deadline_ms: Date.now() + 60000, action: { type: normal ? 'force_stop' : 'mcp_status' } });
  let terminal;
  if (acknowledge) {
    terminal = await adopted.next((frame) => frame.body.type === 'event' && frame.body.observation.type === 'process_exited');
    await adopted.send({ type: 'command', fence, execution, command_id: 'before-terminal-ack', deadline_ms: Date.now() + 60000, action: { type: 'mcp_status' } });
    const refused = await adopted.next((frame) => frame.body.type === 'receipt' && frame.body.command_id === 'before-terminal-ack' && frame.body.outcome.status === 'rejected');
    assert.equal(refused.body.outcome.code, 'host_reconciliation_required', 'the ended host accepts no more native effects');
    assert.equal(child.exitCode, null, 'the owner can acknowledge terminal evidence before host shutdown');
    assert.equal(child.signalCode, null);
    assert.equal(socket.destroyed, false);
    const ack = (cursor) => adopted.send({ type: 'acknowledge', fence, execution, host_instance_id: config.host_instance_id, owner_generation: 2, cursor });
    if (progress) {
      clock = createConnection(join(root, 'clock'));
      const replies = createInterface({ input: clock })[Symbol.asyncIterator]();
      const advance = async (tick, afterAck = 0) => {
        await new Promise((resolve, reject) => clock.write(JSON.stringify({ tick, afterAck }) + '\n', (error) => error ? reject(error) : resolve()));
        const reply = await replies.next();
        assert.equal(reply.done, false, 'the renewed host remains available for the explicit clock barrier');
        return JSON.parse(reply.value);
      };
      assert(terminal.body.cursor > 3);
      assert.equal((await advance(3750)).renewals, 0);
      const first = terminal.body.cursor - 3;
      await ack(first);
      assert.equal((await advance(3750, first)).renewals, 1, 'advancing ACK renews the inactivity deadlines past their original expiry');
      await ack(first);
      await adopted.send({ type: 'command', fence, execution, command_id: 'duplicate-ack-barrier', deadline_ms: Date.now() + 60000, action: { type: 'mcp_status' } });
      await adopted.next((frame) => frame.body.type === 'receipt' && frame.body.command_id === 'duplicate-ack-barrier' && frame.body.outcome.status === 'rejected');
      assert.equal((await advance(0, first)).renewals, 1, 'a duplicate ACK does not renew either deadline');
      await ack(first + 1);
      assert.equal((await advance(3750, first + 1)).renewals, 2);
      await ack(first + 2);
      assert.equal((await advance(3750, first + 2)).renewals, 3, 'progress also renews the independent watchdog past its original expiry');
    }
    await ack(terminal.body.cursor);
  }
  assert.deepEqual(await exited, [null, 'SIGKILL'], stderr);
  assert.deepEqual(await watched, [0, null], 'the host and its surviving native command both ended');
  if (cleanupProof) assert.match(stderr, /Ownership retained through failed cleanup/);
  const journal = readFileSync(join(root, 'outbox'));
  const records = [];
  for (let offset = 0; offset < journal.length;) { const size = journal.readUInt32BE(offset); offset += 4; records.push(parsePayload(journal.subarray(offset, offset + size))); offset += size; }
  assert(records.some((frame) => frame.body.type === 'event' && frame.body.observation.type === 'process_exited' && (normal ? frame.body.observation.signal === 'SIGTERM' : frame.body.observation.exit_code === 23)), 'native exit remains durable for the replacement owner');
  if (acknowledge) assert(records.some((frame) => frame.body.type === 'acknowledge' && frame.body.cursor >= terminal.body.cursor), 'terminal ACK persists before the old group ends');
}
