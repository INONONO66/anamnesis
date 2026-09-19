import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { createInterface } from 'node:readline';
import { mkdtemp, rm, stat, readFile, readdir, mkdir, open } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const bundles = resolve(process.env.UPLOAD_BUNDLE_ROOT ?? '.omo/evidence/upload-lifecycle');
const { RpcClient } = await import(pathToFileURL(bundles + '/client.mjs'));
const HOUR = 3_600_000, CHUNK = 512 * 1024;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const expected = bytes => ({ sha256: hash(bytes), size: bytes.length, media_type: 'application/octet-stream' });
function event(emitter, name, predicate = () => true) {
  return new Promise((resolve, reject) => {
    const done = (error, value) => { clearTimeout(timer); emitter.off(name, observe); error ? reject(error) : resolve(value); };
    const observe = value => { if (predicate(value)) done(null, value); };
    const timer = setTimeout(() => done(Error('deadline: ' + name)), 10000);
    emitter.on(name, observe);
  });
}
async function fixture(run, { hooked = true, prepare = async () => {} } = {}) {
  const root = await mkdtemp('/tmp/ana-upload-uds-'), peers = [], children = [];
  let current;
  async function start() {
    const signals = new EventEmitter(), history = [], listening = event(signals, 'listening');
    const child = spawn(process.execPath, [bundles + (hooked ? '/upload-fixture.mjs' : '/main.mjs')], {
      env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: 'upload-token', ANAMNESIS_NEO4J_PASSWORD: 'unused', ANAMNESIS_NEO4J_URI: 'bolt://127.0.0.1:1' },
      stdio: ['ignore', 'pipe', 'pipe', ...(hooked ? ['ipc'] : [])],
    });
    children.push(child); let errors = '';
    child.stderr.on('data', bytes => { errors += bytes; });
    child.on('message', value => { history.push(value); signals.emit(value.event, value); });
    createInterface({ input: child.stdout }).on('line', line => { const value = JSON.parse(line); signals.emit(value.event, value); });
    const exited = once(child, 'exit');
    await Promise.race([listening, exited.then(([code]) => { throw Error(`daemon exited ${code}: ${errors}`); })]);
    current = { child, signals, history, async command(command, data = {}) {
      const done = event(signals, 'command', x => x.command === command); child.send({ command, ...data }); return done;
    }, async kill() { const done = once(child, 'exit', { signal: AbortSignal.timeout(10000) }); child.kill('SIGKILL'); await done; } };
    return current;
  }
  const connect = async () => { const peer = await RpcClient.connect(root + '/anamnesis.sock', 'upload-token'); peers.push(peer); return peer; };
  try { await prepare(root); await start(); await run({ root, connect, start, get daemon() { return current; } }); }
  finally {
    for (const peer of peers) await peer.close();
    for (const child of children) if (child.exitCode === null && child.signalCode === null) {
      const done = once(child, 'exit', { signal: AbortSignal.timeout(10000) }); child.kill('SIGKILL'); await done;
    }
    await rm(root, { recursive: true, force: true }); await assert.rejects(stat(root), { code: 'ENOENT' });
    console.log(JSON.stringify({ event: 'cleanup', root, removed: true, hooked }));
  }
}
const chunk = (peer, id, seq, bytes) => peer.request('object.chunk', { upload_id: id, seq, bytes_b64: bytes.toString('base64') });

test('unhooked Node UDS maximum chunk, durable adoption, ownership and crash/restart invalidation', { timeout: 30000 }, () => fixture(async ctx => {
  let peer = await ctx.connect();
  const bytes = Buffer.alloc(CHUNK, 73), params = expected(bytes), a = await peer.request('object.begin', params);
  assert.equal(a.chunk_bytes_max, CHUNK);
  assert.equal((await chunk(peer, a.upload_id, 0, bytes)).next_seq, 1);
  assert.equal((await peer.request('object.commit', { upload_id: a.upload_id })).hash, params.sha256);
  assert.deepEqual(await readFile(`${ctx.root}/objects/${params.sha256.slice(0, 2)}/${params.sha256}`), bytes);
  assert.equal((await peer.request('object.begin', params)).state, 'committed');
  await assert.rejects(peer.request('object.begin', { ...params, media_type: 'text/plain' }), { code: 'object_metadata_conflict' });
  const pending = expected(Buffer.from('abandoned-prefix'));
  const old = await peer.request('object.begin', pending);
  await chunk(peer, old.upload_id, 0, Buffer.from('abandoned'));
  assert.equal((await stat(ctx.root + '/uploads/' + old.upload_id)).size, 9);
  const other = await ctx.connect();
  await assert.rejects(chunk(other, old.upload_id, 1, Buffer.from('-prefix')), { code: 'upload_not_found' });
  await ctx.daemon.kill(); await ctx.start(); peer = await ctx.connect();
  assert.deepEqual(await readdir(ctx.root + '/uploads'), []);
  await assert.rejects(peer.request('object.commit', { upload_id: old.upload_id }), { code: 'upload_not_found' });
  await assert.rejects(chunk(peer, old.upload_id, 1, Buffer.from('-prefix')), { code: 'upload_not_found' });
  assert.equal((await peer.request('object.begin', params)).state, 'committed');
  const fresh = await peer.request('object.begin', pending); assert.notEqual(fresh.upload_id, old.upload_id);
}, { hooked: false }));

for (const mode of ['enospc', 'short', 'zero']) test(`Node UDS ${mode} prefix write is rolled back before error, exact maximum-chunk retry verifies final hash`, { timeout: 20000 }, () => fixture(async ctx => {
  const peer = await ctx.connect(), bytes = Buffer.concat([Buffer.from('verified-prefix'), Buffer.alloc(CHUNK, 41)]), a = await peer.request('object.begin', expected(bytes));
  await chunk(peer, a.upload_id, 0, bytes.subarray(0, 15));
  const before = (await ctx.daemon.command('snapshot')).uploads;
  await ctx.daemon.command('arm', { mode });
  const written = event(ctx.daemon.signals, 'prefix-written', x => x.id === a.upload_id);
  await assert.rejects(chunk(peer, a.upload_id, 1, bytes.subarray(15)), { code: 'resource_exhausted' });
  assert.equal((await written).bytes, 2);
  assert.deepEqual(await readFile(ctx.root + '/uploads/' + a.upload_id), bytes.subarray(0, 15));
  assert.deepEqual((await ctx.daemon.command('snapshot')).uploads, before);
  assert.equal((await chunk(peer, a.upload_id, 1, bytes.subarray(15))).next_seq, 2);
  assert.equal((await peer.request('object.commit', { upload_id: a.upload_id })).hash, hash(bytes));
  assert.deepEqual(await readFile(`${ctx.root}/objects/${hash(bytes).slice(0, 2)}/${hash(bytes)}`), bytes);
  assert.deepEqual(await readdir(ctx.root + '/uploads'), []);
}));

test('Node UDS exact expiry cannot race an in-flight chunk; one serial timer releases both slots', { timeout: 20000 }, () => fixture(async ctx => {
  const peer = await ctx.connect(), bytes = Buffer.from('ab'), params = expected(bytes);
  const a = await peer.request('object.begin', params), b = await peer.request('object.begin', params);
  await ctx.daemon.command('advance', { now: HOUR - 1 });
  await ctx.daemon.command('arm', { mode: 'park' });
  const parked = event(ctx.daemon.signals, 'parked');
  const pending = chunk(peer, a.upload_id, 0, bytes.subarray(0, 1)); await parked;
  const expired = event(ctx.daemon.signals, 'expiry-finished');
  const advanced = await ctx.daemon.command('advance', { now: HOUR });
  assert.equal(advanced.active, 1); assert.equal(advanced.timers, 0);
  assert.equal((await readdir(ctx.root + '/uploads')).length, 2);
  assert.equal(ctx.daemon.history.filter(x => x.event === 'expiry-finished').length, 0);
  await ctx.daemon.command('release'); assert.equal((await pending).next_seq, 1);
  const done = await expired; assert.equal(done.maxActive, 1); assert.deepEqual(done.uploads, []);
  assert.deepEqual(await readdir(ctx.root + '/uploads'), []);
  for (const id of [a.upload_id, b.upload_id]) {
    await assert.rejects(chunk(peer, id, 1, bytes.subarray(1)), { code: 'upload_not_found' });
    await assert.rejects(peer.request('object.commit', { upload_id: id }), { code: 'upload_not_found' });
  }
  await peer.request('object.begin', params); await peer.request('object.begin', params);
  const closed = event(ctx.daemon.signals, 'disconnect-finished'); await peer.close();
  assert.deepEqual((await closed).uploads, []); assert.deepEqual(await readdir(ctx.root + '/uploads'), []);
  const other = await ctx.connect(); await other.request('object.begin', params);
  const uploadsClosed = event(ctx.daemon.signals, 'close-finished');
  const exited = once(ctx.daemon.child, 'exit', { signal: AbortSignal.timeout(10000) });
  await other.request('shutdown', {});
  assert.equal((await uploadsClosed).timers, 0); assert.equal((await exited)[0], 0);
}));

test('Node UDS rollback sync failure keeps reservation, rejects retries and releases on connection close', { timeout: 20000 }, () => fixture(async ctx => {
  const peer = await ctx.connect(), bytes = Buffer.from('verified-tail'), params = expected(bytes);
  const a = await peer.request('object.begin', params); await peer.request('object.begin', params);
  await chunk(peer, a.upload_id, 0, bytes.subarray(0, 8));
  await ctx.daemon.command('arm', { mode: 'rollback-failure' });
  await assert.rejects(chunk(peer, a.upload_id, 1, bytes.subarray(8)), { code: 'resource_exhausted' });
  const snapshot = await ctx.daemon.command('snapshot'), failed = snapshot.uploads.find(u => u.id === a.upload_id);
  assert.equal(snapshot.uploads.length, 2); assert.equal(failed.failed, true); assert.equal(failed.actual, bytes.length);
  assert.equal(failed.next, 1); assert.equal(failed.digest, hash(bytes.subarray(0, 8)));
  assert.deepEqual(await readFile(ctx.root + '/uploads/' + a.upload_id), bytes.subarray(0, 8));
  await assert.rejects(chunk(peer, a.upload_id, 1, bytes.subarray(8)), { code: 'resource_exhausted' });
  await assert.rejects(peer.request('object.begin', params), { code: 'resource_exhausted' });
  const disconnected = event(ctx.daemon.signals, 'disconnect-finished'); await peer.close();
  assert.deepEqual((await disconnected).uploads, []); assert.deepEqual(await readdir(ctx.root + '/uploads'), []);
  const other = await ctx.connect(); await other.request('object.begin', params);
}));

test('unhooked Node UDS startup retains and charges unknown surviving bytes before admission', { timeout: 20000 }, () => fixture(async ctx => {
  const peer = await ctx.connect();
  await assert.rejects(peer.request('object.begin', expected(Buffer.from('x'))), { code: 'resource_exhausted' });
  assert.equal((await stat(ctx.root + '/uploads/user-data')).size, 1024 * 1024 * 1024);
}, { hooked: false, prepare: async root => {
  await mkdir(root + '/uploads', { mode: 0o700 });
  const file = await open(root + '/uploads/user-data', 'wx', 0o600);
  try { await file.truncate(1024 * 1024 * 1024); } finally { await file.close(); }
} }));
