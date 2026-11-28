import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { connect as connectSocket } from 'node:net';
import { createInterface } from 'node:readline';
import { mkdtemp, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const bundles = resolve(process.env.RUNTIME_FAIR_BUNDLE_ROOT ?? '.omo/evidence/runtime-fair');
const { RpcClient } = await import(pathToFileURL(`${bundles}/fair-client.mjs`));
function event(signals, name, count = 1) {
  return new Promise((resolve, reject) => {
    const values = [];
    const finish = (error, value) => { clearTimeout(timer); signals.off(name, observe); error ? reject(error) : resolve(value); };
    const observe = value => { values.push(value); if (values.length === count) finish(null, count === 1 ? value : values); };
    const timer = setTimeout(() => finish(Error(`deadline for ${name}`)), 10000);
    signals.on(name, observe);
  });
}
async function fixture(run, park = false) {
  const root = await mkdtemp('/tmp/ana-fair-'); let child; const clients = [];
  try {
    const signals = new EventEmitter();
    const listening = event(signals, 'listening');
    child = spawn(process.execPath, [`${bundles}/fair-fixture.mjs`], { env: { ...process.env, FAIR_PARK: park ? '1' : '0', ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: 'fair-token', ANAMNESIS_NEO4J_PASSWORD: 'unused', ANAMNESIS_NEO4J_URI: 'bolt://127.0.0.1:1' }, stdio: ['ignore', 'pipe', 'pipe', 'ipc'] });
    let output = ''; child.stderr.on('data', b => { output += b; });
    child.on('message', message => signals.emit(message.event, message));
    const lines = createInterface({ input: child.stdout });
    lines.on('line', line => { const message = JSON.parse(line); signals.emit(message.event, message); });
    child.once('exit', (code, signal) => signals.emit('exit', { code, signal, output }));
    await listening;
    const connect = async () => { const client = await RpcClient.connect(root + '/anamnesis.sock', 'fair-token'); clients.push(client); return client; };
    const recover = async () => { const recovered = event(signals, 'recovered'); child.send('recover'); await recovered; };
    await run({ root, child, signals, connect, recover });
  } finally {
    for (const client of clients) await client.close();
    if (child && child.exitCode === null && child.signalCode === null) { const exited = once(child, 'exit', { signal: AbortSignal.timeout(10000) }); child.kill('SIGKILL'); await exited; }
    await rm(root, { recursive: true, force: true });
  }
}
test('daemon returns control before handling a complete embedding cohort', { timeout: 20000 }, () => fixture(async ({ connect, recover, signals }) => {
  const client = await connect(); await recover();
  const control = event(signals, 'control-return'); const settled = event(signals, 'fixture-settled');
  const response = client.request('status', {});
  const observed = await control; await response;
  assert.ok(observed.handled < 12, `control held until all ${observed.handled} embedding entries were handled`);
  await settled;
}));
test('saturated foreground dispatch cannot starve serialized background turns', { timeout: 20000 }, () => fixture(async ({ child, signals, connect, recover }) => {
  const clients = []; for (let i = 0; i < 16; i++) clients.push(await connect());
  await recover();
  const parked = event(signals, 'parked');
  await clients[0].request('status', {}); await parked;
  // All 256 foreground slots are admitted while the first embedding turn is held.
  const admitted = event(signals, 'admitted', 256);
  const controls = event(signals, 'control-return', 256);
  const settled = event(signals, 'fixture-settled');
  const responses = clients.flatMap(client => Array.from({ length: 16 }, () => client.request('status', {})));
  await admitted; child.send('release');
  const observed = await controls; await Promise.all(responses); await settled;
  assert.ok(observed[3].handled > 0, 'background must progress while foreground remains saturated');
  assert.ok(observed[0].handled < 12, 'control must interleave with background');
  assert.equal(observed.at(-1).handled, 12);
}, true));
test('authenticated control retains headroom; pre-hello status cannot spend it', { timeout: 20000 }, () => fixture(async ({ root, child, signals, connect, recover }) => {
  const client = await connect(); await recover();
  const parked = event(signals, 'parked'); await client.request('status', {}); await parked;
  const admitted = event(signals, 'admitted', 32);
  const replies = Array.from({ length: 14 }, () => client.request('object.commit', { upload_id: '00000000-0000-4000-8000-000000000001' }).catch(error => error));
  const control = client.request('status', {});
  const over = client.request('object.commit', { upload_id: '00000000-0000-4000-8000-000000000001' }).catch(error => error);
  const socket = connectSocket(root + '/anamnesis.sock');
  let buffer = Buffer.alloc(0), received = [];
  const raw = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Error('raw response deadline')), 10000);
    socket.on('data', bytes => {
      buffer = Buffer.concat([buffer, bytes]);
      while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
        const size = 4 + buffer.readUInt32BE(); received.push(JSON.parse(buffer.subarray(4, size))); buffer = buffer.subarray(size);
        if (received.length === 16) { clearTimeout(timer); resolve(received); }
      }
    });
    socket.on('error', error => { clearTimeout(timer); reject(error); });
  });
  try {
    for (let id = 1; id <= 16; id++) {
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id, method: 'status', params: {} }));
      const bytes = Buffer.alloc(4 + body.length); bytes.writeUInt32BE(body.length); body.copy(bytes, 4); socket.write(bytes);
    }
    await admitted; child.send('release');
    assert.equal((await over).code, 'resource_exhausted');
    assert.equal((await control).storage, 'available');
    for (const error of await Promise.all(replies)) assert.equal(error.code, 'upload_not_found');
    const values = await raw;
    assert.equal(values.filter(value => value.error.data.code === 'unauthenticated').length, 14);
    assert.equal(values.filter(value => value.error.data.code === 'resource_exhausted').length, 2);
  } finally { socket.destroy(); }
}, true));
test('shutdown cancels queued background continuations', { timeout: 20000 }, () => fixture(async ({ child, signals, connect, recover }) => {
  const client = await connect(); await recover();
  const parked = event(signals, 'parked'); await client.request('status', {}); await parked;
  const exited = event(signals, 'exit');
  const admitted = event(signals, 'admitted'); const response = client.request('shutdown', {});
  let settled = false; signals.once('fixture-settled', () => { settled = true; });
  await admitted; child.send('release');
  assert.equal((await response).state, 'stopping'); assert.equal((await exited).code, 0);
  assert.equal(settled, false);
}, true));
