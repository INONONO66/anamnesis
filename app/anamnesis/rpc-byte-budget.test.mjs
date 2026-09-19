import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { connect } from 'node:net';
import { createInterface } from 'node:readline';
import { mkdtemp, rm, stat } from 'node:fs/promises';
import { resolve } from 'node:path';
const bundles = resolve(process.env.RPC_BYTE_BUNDLE_ROOT ?? '.omo/evidence/rpc-byte-budget');
const M = 1024 * 1024, F = M + 4, S = 4100;
const caps = { general: 256 * (F + S), control: 8 * (F + S), ingress: 64 * (S + 4096) };
const connectionCaps = { general: 16 * (F + S), control: 2 * (F + S), ingress: S + 4096 };
const zero = { general: 0, control: 0, ingress: 0 };
const hello = { token: 'byte-token', client: 'byte-fixture', version: 1, commit_mode: 'receipt' };
function frame(method, id, params = {}, size) {
  let body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id, method, params }));
  if (size) body = Buffer.concat([body, Buffer.alloc(size - body.length, 32)]);
  const bytes = Buffer.alloc(body.length + 4); bytes.writeUInt32BE(body.length); body.copy(bytes, 4); return bytes;
}
function event(emitter, name, predicate = () => true) {
  return new Promise((resolve, reject) => {
    const done = (error, value) => { clearTimeout(timer); emitter.off(name, observe); error ? reject(error) : resolve(value); };
    const observe = value => { if (predicate(value)) done(null, value); };
    const timer = setTimeout(() => done(Error(`deadline: ${name}`)), 10000);
    emitter.on(name, observe);
  });
}
async function fixture(run) {
  const root = await mkdtemp('/tmp/ana-bytes-'), signals = new EventEmitter(), peers = [];
  const listening = event(signals, 'listening');
  const child = spawn(process.execPath, [`${bundles}/byte-fixture.mjs`], { env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: 'byte-token', ANAMNESIS_NEO4J_PASSWORD: 'unused', ANAMNESIS_NEO4J_URI: 'bolt://127.0.0.1:1' }, stdio: ['ignore', 'pipe', 'pipe', 'ipc'] });
  const history = []; let errors = '';
  child.stderr.on('data', b => { errors += b; });
  child.on('message', value => { history.push(value); signals.emit(value.event, value); });
  createInterface({ input: child.stdout }).on('line', line => { const value = JSON.parse(line); signals.emit(value.event, value); });
  child.on('exit', (code, signal) => signals.emit('exit', { code, signal, errors }));
  const command = async name => { const done = event(signals, 'command', x => x.command === name); child.send(name); return done; };
  const peer = async (auth = true) => {
    const socket = connect(root + '/anamnesis.sock'), replies = new EventEmitter(); let buffer = Buffer.alloc(0);
    socket.on('error', error => replies.emit('failure', error));
    socket.on('data', bytes => {
      buffer = Buffer.concat([buffer, bytes]);
      while (buffer.length >= 4 && buffer.length >= buffer.readUInt32BE() + 4) {
        const size = buffer.readUInt32BE() + 4, value = JSON.parse(buffer.subarray(4, size));
        buffer = buffer.subarray(size); replies.emit('reply', value);
      }
    });
    const p = { socket, replies, async request(method, id, params) { const response = event(replies, 'reply', x => x.id === id); socket.write(frame(method, id, params)); return response; } };
    peers.push(p); await once(socket, 'connect', { signal: AbortSignal.timeout(10000) });
    if (auth) assert.equal((await p.request('hello', 0, hello)).result.principal, 'installation');
    return p;
  };
  const input = async (p, bytes) => {
    // Only this peer writes during this exchange. Count actual socket bytes,
    // so transport fragmentation/coalescing cannot accidentally satisfy it.
    let received = 0;
    const done = event(signals, 'input', x => (received += x.bytes) >= bytes.length);
    p.socket.write(bytes); return done;
  };
  try {
    await listening; await run({ root, child, signals, history, command, peer, input });
    const accounting = history.filter(x => x.event === 'accounting');
    for (const x of accounting) for (const pool of Object.keys(caps)) {
      assert.ok(x.pools[pool] >= 0 && x.pools[pool] <= caps[pool], `${pool} global bound`);
      assert.equal(x.pools[pool], x.accounts.reduce((sum, a) => sum + a[pool], 0), `${pool} aggregate sum`);
      for (const a of x.accounts) assert.ok(a[pool] >= 0 && a[pool] <= connectionCaps[pool], `${pool} connection bound`);
    }
    console.log(JSON.stringify({ event: 'invariants', observations: accounting.length, peaks: Object.fromEntries(Object.keys(caps).map(pool => [pool, Math.max(0, ...accounting.map(x => x.pools[pool]))])) }));
  }
  finally {
    for (const p of peers) p.socket.destroy();
    if (child.exitCode === null && child.signalCode === null) { const exited = once(child, 'exit', { signal: AbortSignal.timeout(10000) }); child.kill('SIGKILL'); await exited; }
    await rm(root, { recursive: true, force: true }); await assert.rejects(stat(root), { code: 'ENOENT' });
    console.log(JSON.stringify({ event: 'cleanup', root, removed: true }));
  }
}
const commit = { upload_id: '00000000-0000-4000-8000-000000000001' };
async function park(ctx, p) {
  await ctx.command('arm'); const parked = event(ctx.signals, 'parked');
  p.socket.write(frame('object.commit', 1, commit, M)); await parked;
}

test('fragmented multi-peer inputs cannot allocate beyond connection/global encoded reservations', { timeout: 30000 }, () => fixture(async ctx => {
  const peers = []; for (let i = 0; i < 16; i++) peers.push(await ctx.peer());
  // Only the seventeenth peer sends input during this hello exchange.
  const helloInput = event(ctx.signals, 'input');
  peers.push(await ctx.peer());
  let { parser } = await helloInput;
  await park(ctx, peers[0]);
  for (let i = 0; i < 16; i++) {
    for (let j = i === 0 ? 1 : 0; j < 8; j++) {
      const bytes = frame('object.commit', 10 + j, commit, M);
      await ctx.input(peers[i], bytes.subarray(0, 2));
      await ctx.input(peers[i], bytes.subarray(2, 4));
      await ctx.input(peers[i], bytes.subarray(4));
    }
  }
  const before = await ctx.command('snapshot'); assert.equal(before.allocations, 128);
  // A real cork delays the actual error-write callback. Parser completion is
  // deliberately NOT the release boundary, even after allocation was denied.
  await ctx.command('cork-writes');
  let errorReservation;
  const output = event(ctx.signals, 'accounting', x => {
    if (x.operation !== 'output' || x.parser !== parser || x.reservation?.input !== 0 || x.reservation?.actual <= 0) return false;
    errorReservation = x.reservation.id; return true;
  });
  const released = event(ctx.signals, 'accounting', x => x.operation === 'release'
    && errorReservation !== undefined && x.reservation?.id === errorReservation);
  const errorReply = event(peers[16].replies, 'reply', x => x.id === null);
  const queued = event(ctx.signals, 'write-queued', x => x.corked > 0);
  const header = Buffer.alloc(4); header.writeUInt32BE(M);
  const fragmentedHeader = event(ctx.signals, 'input', x => x.parser !== undefined);
  await ctx.input(peers[16], header.subarray(0, 2));
  parser = (await fragmentedHeader).parser;
  const after = await ctx.input(peers[16], header.subarray(2));
  assert.equal(after.parser, parser);
  assert.equal(after.allocations, 128, 'global saturation must reject the next body BEFORE allocation');
  const charged = await output, write = await queued;
  assert.equal(write.writableLength, charged.reservation.actual);
  assert.equal(write.accepted, false);
  assert.equal(charged.reservation.live, true);
  assert.ok(charged.reservation.actual > 0);
  assert.equal(charged.pools.general, 256 * F + charged.reservation.actual);
  assert.equal(ctx.history.some(x => x.event === 'accounting' && x.operation === 'release'
    && x.reservation?.id === errorReservation), false);
  await ctx.command('uncork-writes');
  const settled = await released;
  assert.equal((await errorReply).error.data.code, 'resource_exhausted');
  assert.equal(settled.reservation.live, false);
  assert.equal(settled.pools.general, 256 * F);
  console.log(JSON.stringify({ event: 'denied-header-release', parser, reservation: errorReservation,
    allocations: after.allocations, charged, write, settled }));
  await ctx.command('release');
}));

test('connection boundary rejects next maximum body before allocation', { timeout: 20000 }, () => fixture(async ctx => {
  const p = await ctx.peer(); await park(ctx, p);
  for (let i = 0; i < 7; i++) await ctx.input(p, frame('object.commit', 10 + i, commit, M));
  const header = Buffer.alloc(4); header.writeUInt32BE(M);
  const after = await ctx.input(p, header);
  assert.equal(after.allocations, 8, 'connection saturation must reject the ninth body BEFORE allocation');
  await ctx.command('release');
}));


test('partial disconnect, queued cancellation and executing disconnect release exactly their ownership', { timeout: 20000 }, () => fixture(async ctx => {
  const a = await ctx.peer(), b = await ctx.peer(), partial = await ctx.peer();
  await park(ctx, a);
  await ctx.input(b, frame('object.commit', 2, commit, M));
  const header = Buffer.alloc(4); header.writeUInt32BE(M);
  await ctx.input(partial, header);
  assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: 6 * F });
  const partialReleased = event(ctx.signals, 'accounting', x => x.operation === 'release' && x.pools.general === 4 * F);
  partial.socket.destroy(); await partialReleased;
  const queuedReleased = event(ctx.signals, 'accounting', x => x.operation === 'release' && x.pools.general === 2 * F);
  b.socket.destroy(); await queuedReleased;
  const closed = event(ctx.signals, 'socket-closed', x => x.id === 1); a.socket.destroy(); await closed;
  assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: 2 * F }, 'running request remains charged after disconnect');
  const allReleased = event(ctx.signals, 'accounting', x => x.operation === 'release' && x.pools.general === 0);
  await ctx.command('release'); await allReleased;
  const final = await ctx.command('snapshot'); assert.deepEqual(final.pools, zero); assert.equal(final.maxActive, 1);
  assert.equal(ctx.history.filter(x => x.event === 'mutation-finished').length, 1, 'cancelled queued mutation never executes');
}));

test('real Socket backpressure replaces allowance with actual output until callback; failed writes release once', { timeout: 20000 }, () => fixture(async ctx => {
  const p = await ctx.peer(); await ctx.command('cork-writes');
  const queued = event(ctx.signals, 'write-queued', x => x.corked > 0);
  const response = event(p.replies, 'reply', x => x.id === 20);
  const input = frame('status', 20); p.socket.write(input);
  const output = await queued;
  assert.equal(output.accepted, false); assert.equal(output.writableLength, output.bytes);
  const snapshot = await ctx.command('snapshot');
  assert.deepEqual(snapshot.pools, { ...zero, general: input.length + output.bytes });
  const charge = ctx.history.filter(x => x.event === 'accounting' && x.operation === 'output').at(-1).reservation;
  assert.equal(charge.allowance, 0); assert.equal(charge.actual, output.bytes); assert.equal(charge.input, input.length);
  const released = event(ctx.signals, 'accounting', x => x.operation === 'release' && x.pools.general === 0);
  await ctx.command('uncork-writes'); await released;
  assert.equal((await response).result.storage, 'unavailable');
  await ctx.command('cork-writes');
  const blocked = event(ctx.signals, 'write-queued', x => x.corked > 0);
  p.socket.write(frame('status', 21)); await blocked;
  const failed = event(ctx.signals, 'write-ready', x => x.id === 21 && x.error !== null);
  const cleaned = event(ctx.signals, 'socket-closed');
  await ctx.command('fail-writes'); await failed; await cleaned;
  assert.deepEqual((await ctx.command('snapshot')).pools, zero);
}));

test('parse/UTF-8/schema rejection and invalid length retain only reserved error output until completion', { timeout: 20000 }, () => fixture(async ctx => {
  const p = await ctx.peer();
  await ctx.command('hold-writes');
  for (const body of [Buffer.from('{'), Buffer.from([255]), Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: 1, method: 'status', params: { extra: true } }))]) {
    const bytes = Buffer.alloc(body.length + 4); bytes.writeUInt32BE(body.length); body.copy(bytes, 4);
    const ready = event(ctx.signals, 'write-ready', x => x.id === null);
    const reply = event(p.replies, 'reply', x => x.id === null);
    await ctx.input(p, bytes); const output = await ready;
    assert.ok(['parse_error', 'invalid_params'].includes((await reply).error.data.code));
    assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: bytes.length + output.bytes });
    await ctx.command('finish-writes'); assert.deepEqual((await ctx.command('snapshot')).pools, zero);
    await ctx.command('hold-writes');
  }
  const header = Buffer.alloc(4); header.writeUInt32BE(M + 1);
  const ready = event(ctx.signals, 'write-ready', x => x.id === null);
  const reply = event(p.replies, 'reply', x => x.id === null);
  const observed = await ctx.input(p, header); await ready;
  assert.equal(observed.allocations, 0); assert.equal((await reply).error.data.code, 'invalid_request');
  await ctx.command('finish-writes'); assert.deepEqual((await ctx.command('snapshot')).pools, zero);
}));

test('authenticated control uses bounded headroom at exact global saturation; pre-hello and non-control cannot', { timeout: 30000 }, () => fixture(async ctx => {
  const peers = []; for (let i = 0; i < 17; i++) peers.push(await ctx.peer());
  const unauthenticated = await ctx.peer(false);
  await park(ctx, peers[0]);
  for (let i = 0; i < 16; i++) for (let j = i === 0 ? 1 : 0; j < 8; j++) await ctx.input(peers[i], frame('object.commit', 10 + j, commit, M));
  // Fill the precise remaining general allowance (F + 1020 encoded bytes).
  await ctx.input(peers[16], frame('object.commit', 30, commit, 1016));
  assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: caps.general });
  const deniedClose = once(unauthenticated.socket, 'close', { signal: AbortSignal.timeout(10000) });
  await ctx.input(unauthenticated, frame('status', 40).subarray(0, 4)); await deniedClose;
  assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: caps.general });
  await ctx.command('hold-writes');
  const deniedReply = event(peers[16].replies, 'reply', x => x.id === 41);
  const errorReady = event(ctx.signals, 'write-ready', x => x.id === 41);
  const nonControl = frame('object.commit', 41, commit);
  await ctx.input(peers[16], nonControl); const error = await errorReady;
  assert.equal((await deniedReply).error.data.code, 'resource_exhausted');
  assert.deepEqual((await ctx.command('snapshot')).pools, { general: caps.general, control: 0, ingress: nonControl.length + error.bytes });
  await ctx.command('finish-writes');
  // Four peers, two small controls each: exactly eight reserved control slots.
  const controls = [];
  let controlBytes = 0;
  for (let i = 0; i < 4; i++) for (let j = 0; j < 2; j++) {
    const id = 100 + i * 2 + j, bytes = frame('status', id);
    controls.push(event(peers[i].replies, 'reply', x => x.id === id));
    await ctx.input(peers[i], bytes); controlBytes += F + bytes.length;
  }
  assert.deepEqual((await ctx.command('snapshot')).pools, { general: caps.general, control: controlBytes, ingress: 0 });
  const overflow = event(peers[4].replies, 'reply', x => x.id === 200);
  await ctx.input(peers[4], frame('status', 200)); assert.equal((await overflow).error.data.code, 'resource_exhausted');
  await ctx.command('release');
  for (const response of await Promise.all(controls)) assert.equal(response.result.storage, 'unavailable');
  // Serial status is a queue barrier behind the earlier mutation admissions.
  assert.equal((await peers[16].request('status', 300)).result.storage, 'unavailable');
  assert.equal((await ctx.command('snapshot')).maxActive, 1);
}));

test('count rejection reserves error output without dropping already queued frame bytes', { timeout: 20000 }, () => fixture(async ctx => {
  const p = await ctx.peer(); await park(ctx, p);
  let inputs = F;
  for (let id = 2; id <= 14; id++) { const bytes = frame('object.commit', id, commit); inputs += bytes.length; await ctx.input(p, bytes); }
  await ctx.command('hold-writes');
  const ready = event(ctx.signals, 'write-ready', x => x.id === 15);
  const response = event(p.replies, 'reply', x => x.id === 15);
  const rejected = frame('object.commit', 15, commit); await ctx.input(p, rejected);
  const output = await ready; assert.equal((await response).error.data.code, 'resource_exhausted');
  assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: inputs + 14 * F + rejected.length + output.bytes });
  await ctx.command('finish-writes');
  assert.deepEqual((await ctx.command('snapshot')).pools, { ...zero, general: inputs + 14 * F });
  await ctx.command('release');
  assert.equal((await p.request('status', 16)).result.storage, 'unavailable');
  assert.equal((await ctx.command('snapshot')).maxActive, 1);
}));

test('built daemon shutdown releases partial frames and all response reservations', { timeout: 20000 }, () => fixture(async ctx => {
  const p = await ctx.peer(), partial = await ctx.peer();
  const header = Buffer.alloc(4); header.writeUInt32BE(M);
  await ctx.input(partial, header);
  const exited = event(ctx.signals, 'exit');
  assert.equal((await p.request('shutdown', 70)).result.state, 'stopping');
  assert.equal((await exited).code, 0);
  assert.deepEqual(ctx.history.filter(x => x.event === 'accounting').at(-1).pools, zero);
  await assert.rejects(stat(ctx.root + '/owner'), { code: 'ENOENT' });
  await assert.rejects(stat(ctx.root + '/anamnesis.sock'), { code: 'ENOENT' });
}));
