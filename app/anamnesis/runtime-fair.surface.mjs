// Fresh owner-checked Neo4j, built Node daemon and UDS. Only the parked
// completion entry uses fixture hooks, all DB/spool methods remain real.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { createServer, connect } from 'node:net';
import { createInterface } from 'node:readline';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, readFile, rm, writeFile, appendFile, stat } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import neo4j from 'neo4j-driver';
const bundles = resolve(process.env.RUNTIME_FAIR_BUNDLE_ROOT ?? '.omo/evidence/runtime-fair-drain');
const evidence = resolve(process.env.RUNTIME_FAIR_EVIDENCE_ROOT ?? `${bundles}/owned-db`);
await mkdir(evidence, { recursive: true });
const { RpcClient } = await import(pathToFileURL(`${bundles}/page-client.mjs`));
const owner = randomUUID(), name = `anamnesis-fair-${owner}`;
const password = randomBytes(24).toString('base64url'), token = randomBytes(24).toString('base64url');
const roots = [], children = new Set(), clients = new Set(), sockets = new Set();
let root, targetPort = 1, dbPort, daemon, attachment, driver, attempted = false, failure, logs = Promise.resolve();
const redact = text => text.replaceAll(password, '[REDACTED]').replaceAll(token, '[REDACTED]');
const record = (file, value) => writeFile(`${evidence}/${file}`, redact(JSON.stringify(value, null, 2) + '\n'));
function event(handle, name, count = 1) {
  return new Promise((resolve, reject) => {
    const values = [];
    const finish = (error, value) => { clearTimeout(timer); handle.signals.off(name, observe); handle.signals.off('terminal', terminal); error ? reject(error) : resolve(value); };
    const observe = value => { values.push(value); if (values.length === count) finish(null, count === 1 ? value : values); };
    const terminal = () => finish(Error(`process ended before ${name}`));
    const timer = setTimeout(() => finish(Error(`event deadline: ${name}`)), 120000);
    handle.signals.on(name, observe); handle.signals.on('terminal', terminal);
  });
}
function launch(command, args, { env = process.env, ready, ipc = false } = {}) {
  const signals = new EventEmitter();
  const child = spawn(command, args, { env, stdio: ['ignore', 'pipe', 'pipe', ...(ipc ? ['ipc'] : [])] }); children.add(child);
  let output = '', readyResolve;
  const readyPromise = new Promise(resolve => { readyResolve = resolve; });
  const timer = setTimeout(() => { readyResolve(false); child.kill('SIGKILL'); }, 180000);
  child.on('message', value => signals.emit(value.event, value));
  const readers = [child.stdout, child.stderr].map(stream => {
    stream.on('data', bytes => { output += bytes; logs = logs.then(() => appendFile(`${evidence}/processes.txt`, redact(bytes.toString()))); });
    const reader = createInterface({ input: stream });
    reader.on('line', line => {
      if (ready?.test(line)) { clearTimeout(timer); readyResolve(true); }
      let value; try { value = JSON.parse(line); } catch { return; }
      signals.emit(value.event, value);
    }); return reader;
  });
  const done = new Promise((resolve, reject) => {
    child.once('error', reject);
    child.once('close', (code, signal) => { clearTimeout(timer); readyResolve(false); children.delete(child); readers.forEach(r => r.close()); signals.emit('terminal'); resolve({ code, signal, output }); });
  });
  return { child, signals, done, ready: readyPromise };
}
async function command(command, args, options) { const result = await launch(command, args, options).done; assert.equal(result.code, 0, result.output); return result.output.trim(); }
const docker = args => command('docker', args, { env: { ...process.env, NEO4J_AUTH: `neo4j/${password}` } });
const relay = createServer(socket => {
  const upstream = connect(targetPort, '127.0.0.1');
  for (const s of [socket, upstream]) { sockets.add(s); s.once('close', () => sockets.delete(s)); }
  socket.on('error', () => upstream.destroy()); upstream.on('error', () => socket.destroy());
  socket.on('close', () => upstream.destroy()); upstream.on('close', () => socket.destroy());
  socket.pipe(upstream); upstream.pipe(socket);
});
const listening = once(relay, 'listening'); relay.listen(0, '127.0.0.1'); await listening;
const offline = () => { targetPort = 1; for (const socket of sockets) socket.destroy(); };
const hash = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
const key = params => { const o = params.episode.origin; return hash([hash([o.source, o.session, o.actor, o.record]), params.source_revision]); };
const input = (record, revision = 'v1', predecessor = null, source = 'fair-page') => ({ episode: { schema: 'anamnesis.original-message/1', time: { value: '2026-09-09T00:00:00Z', precision: 'second' }, content: `${record}/${revision}`, mass: 1, properties: {}, origin: { source, session: 's', actor: 'a', record } }, source_revision: revision, expected_previous_revision_key: predecessor });
const identity = receipt => ({ revision_key: receipt.revision_key, body_digest: receipt.body_digest, data_incarnation: receipt.data_incarnation });
async function newRoot() { root = await mkdtemp('/tmp/ana-fair-db-'); roots.push(root); await record('resources.json', { owner, name, roots }); }
async function start(fixture = false) {
  daemon = launch(process.execPath, [`${bundles}/${fixture ? 'fair-real-fixture' : 'page-daemon'}.mjs`], { ipc: fixture, ready: /"event":"listening"/, env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_PASSWORD: password, ANAMNESIS_NEO4J_USER: 'neo4j', ANAMNESIS_NEO4J_URI: `bolt://127.0.0.1:${relay.address().port}` } });
  assert.equal(await daemon.ready, true, 'daemon not listening');
  const client = await RpcClient.connect(root + '/anamnesis.sock', token); clients.add(client); return client;
}
async function stop(client) {
  const reply = await client.request('shutdown', {}); assert.equal(reply.state, 'stopping');
  await client.close(); clients.delete(client); const result = await daemon.done; assert.equal(result.code, 0, result.output);
  await assert.rejects(stat(root + '/owner'), { code: 'ENOENT' });
}
async function recover(client) {
  const settled = event(daemon, 'drain_settled'); targetPort = dbPort;
  const first = await client.request('status', {}); assert.equal(first.storage, 'available');
  await settled; return client.request('status', {});
}
async function verifyRows(source, receipts) {
  const result = await driver.executeQuery('MATCH (e:Episode {origin_source:$source}) OPTIONAL MATCH (o:Outbox {element_id:e.id}) RETURN e.revision_key AS revision, e.id AS id, e.ingest_seq AS sequence, count(o) AS outbox', { source });
  const rows = result.records.map(row => row.toObject()); assert.equal(rows.length, receipts.length);
  assert.deepEqual(rows.map(row => row.revision).sort(), receipts.map(row => row.revision_key).sort());
  assert.equal(new Set(rows.map(row => row.id)).size, receipts.length); assert.equal(new Set(rows.map(row => row.sequence)).size, receipts.length);
  for (const row of rows) assert.equal(row.outbox, 1); return rows;
}
try {
  await newRoot(); attempted = true;
  await docker(['create', '--name', name, '--label', `anamnesis.qa.owner=${owner}`, '-p', '127.0.0.1::7687', '-e', 'NEO4J_AUTH', '-e', 'NEO4J_server_memory_heap_max__size=512M', '-e', 'NEO4J_server_memory_pagecache_size=256M', 'neo4j:5.26-community']);
  attachment = launch('docker', ['start', '-a', name], { ready: /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}(?:[+-]\d{4})?\s+INFO\s+Started\.$/ });
  assert.equal(await attachment.ready, true, 'Neo4j did not emit Started');
  dbPort = Number(await docker(['inspect', '-f', '{{(index (index .NetworkSettings.Ports "7687/tcp") 0).HostPort}}', name]));
  driver = neo4j.driver(`bolt://127.0.0.1:${dbPort}`, neo4j.auth.basic('neo4j', password), { disableLosslessIntegers: true }); await driver.verifyConnectivity();
  // Exactly 50 durable UDS receipts survive graceful shutdown and offline restart.
  let client = await start(); const before = await client.request('status', {}); const queued = [];
  for (let i = 0; i < 50; i++) { const params = input(`offline-${i}`, 'v1', null, 'fair-50'); const receipt = await client.request('remember', params); assert.equal(receipt.state, 'spooled'); assert.equal(receipt.spool_seq, i + 1); queued.push({ params, receipt }); }
  await stop(client); client = await start(); const restarted = await client.request('status', {});
  assert.equal(restarted.spool.pending, 50); assert.equal(restarted.data_incarnation, before.data_incarnation); assert.notEqual(restarted.fs_epoch, before.fs_epoch);
  for (const q of queued) assert.deepEqual(await client.request('remember', q.params), q.receipt);
  const drained = await recover(client); assert.equal(drained.spool.pending, 0); assert.equal(drained.outbox_pending, 50);
  const committed = [];
  for (const q of queued) {
    const result = await client.request('ingest.status', identity(q.receipt)); assert.equal(result.state, 'committed'); assert.deepEqual(identity(result), identity(q.receipt));
    const retry = await client.request('remember', q.params); assert.equal(retry.id, result.id); assert.equal(retry.ingest_seq, result.ingest_seq); assert.equal(retry.created, false); committed.push(result);
  }
  await record('50-result.json', { before, restarted, drained, queued, committed, rows: await verifyRows('fair-50', queued.map(q => q.receipt)) }); await stop(client);
  // Separate installation/root, same owned DB, cross-page dependency and gaps.
  await newRoot(); targetPort = dbPort; client = await start(true);
  const staleCurrent = await client.request('remember', input('stale', 'current')); assert.equal(staleCurrent.state, 'committed');
  offline(); const head = await client.request('remember', input('missing', 'a', key(input('missing', 'absent'))));
  const parent = input('chain', 'v1'), child = input('chain', 'v2', key(parent)); const childReceipt = await client.request('remember', child);
  const success = [staleCurrent, childReceipt];
  for (let i = 0; i < 98; i++) success.push(await client.request('remember', input(`filler-${i}`)));
  const parentReceipt = await client.request('remember', parent); success.push(parentReceipt); assert.equal(parentReceipt.spool_seq, 101);
  const blocked = [[head, 'missing_predecessor']]; const b = input('missing', 'b', head.revision_key), c = input('missing', 'c', key(b));
  for (const params of [c, b]) blocked.push([await client.request('remember', params), 'missing_predecessor']);
  const ca = input('cycle', 'a'), cb = input('cycle', 'b', key(ca)); ca.expected_previous_revision_key = key(cb);
  for (const [params, reason] of [[input('cycle', 'tail', key(ca)), 'missing_predecessor'], [ca, 'dependency_cycle'], [cb, 'dependency_cycle']]) blocked.push([await client.request('remember', params), reason]);
  const stale = input('stale', 'a'); blocked.push([await client.request('remember', input('stale', 'b', key(stale))), 'missing_predecessor']); blocked.push([await client.request('remember', stale), 'stale_revision']);
  const armed = event(daemon, 'armed'); daemon.child.send('arm'); await armed;
  const parked = event(daemon, 'completion_parked'); targetPort = dbPort;
  const initial = await client.request('status', {}); assert.equal(initial.spool.pending, 108); const boundary = await parked;
  assert.equal(boundary.sequence, 3);
  // Completion 3 is actually durable while the head and same-origin child wait.
  const heldDone = JSON.parse(JSON.parse(await readFile(root + '/spool/spool.done', 'utf8')).payload);
  assert.equal(heldDone.frontier, 0); assert.deepEqual(heldDone.completed, [3]);
  const admitted = event(daemon, 'admitted'); const control = client.request('status', {}); await admitted;
  daemon.child.send('release'); const interleaved = await control;
  assert.equal(interleaved.outbox_pending, 52, 'control must run after one suffix completion, not the whole cohort');
  // A queued shutdown cancels the rest, then ordinary unhooked Node restart
  // replays the exact admitted entries, including completed suffix sequence 3.
  const journal = await readFile(root + '/spool/spool.journal'); await stop(client);
  assert.deepEqual(await readFile(root + '/spool/spool.journal'), journal);
  offline(); client = await start(); assert.equal((await client.request('status', {})).spool.pending, 108);
  const status = await recover(client); assert.equal(status.spool.pending, 108); assert.equal(status.spool.blocked, 8); assert.equal(status.outbox_pending, 151);
  for (const [receipt, reason] of blocked) assert.equal((await client.request('ingest.status', identity(receipt))).reason, reason);
  const verified = [];
  for (const receipt of success) { const result = await client.request('ingest.status', identity(receipt)); assert.equal(result.state, 'committed'); assert.deepEqual(identity(result), identity(receipt)); verified.push(result); }
  const chain = await driver.executeQuery('MATCH (h:OriginHead {origin_key:$origin}) MATCH (e:Episode {revision_key:$child}) RETURN h.revision_key AS head, e.previous_revision_key AS predecessor', { origin: hash(['fair-page', 's', 'a', 'chain']), child: key(child) });
  assert.equal(chain.records[0].get('head'), key(child)); assert.equal(chain.records[0].get('predecessor'), key(parent));
  const doneBytes = await readFile(root + '/spool/spool.done'); const done = JSON.parse(JSON.parse(doneBytes.toString()).payload);
  assert.equal(done.frontier, 0); assert.deepEqual(done.completed, Array.from({ length: 100 }, (_, i) => i + 2));
  const repeat = await client.request('status', {}); assert.equal(repeat.spool.blocked, 8); assert.deepEqual(await readFile(root + '/spool/spool.done'), doneBytes);
  await assert.rejects(client.request('remember', input('chain', 'bad', key(parent))), { code: 'stale_revision' });
  const absent = await driver.executeQuery('MATCH (e:Episode) WHERE e.revision_key IN $keys RETURN count(e) AS count', { keys: blocked.map(([receipt]) => receipt.revision_key) }); assert.equal(absent.records[0].get('count'), 0);
  await record('page-result.json', { initial, boundary, heldDone, interleaved, status, blocked, verified, rows: await verifyRows('fair-page', success), done });
  await stop(client); await record('result.json', { ok: true, exact50: true, crossPage: true, serializedControlBeforeCohort: true, shutdownRestart: true, exactIdentitiesOutboxCAS: true });
} catch (error) { failure = error; await record('result.json', { ok: false, error: String(error), stack: error.stack }); }
finally {
  const cleanup = [];
  const attempt = async (name, action) => { try { await action(); cleanup.push({ name, ok: true }); } catch (error) { cleanup.push({ name, error: String(error) }); failure = new AggregateError(failure ? [failure, error] : [error], 'cleanup failed'); } };
  await attempt('clients', async () => { for (const client of clients) await client.close(); });
  await attempt('daemon', async () => { if (daemon && children.has(daemon.child)) { daemon.child.kill('SIGKILL'); await daemon.done; } });
  await attempt('driver', async () => { await driver?.close(); });
  await attempt('owned-container', async () => { if (attempted) { const inspected = JSON.parse(await docker(['inspect', '--format', '{{json .}}', name])); assert.equal(inspected.Config.Labels['anamnesis.qa.owner'], owner); await docker(['rm', '-f', '-v', inspected.Id]); cleanup.push({ container: inspected.Id, removed: true }); } if (attachment) await attachment.done; });
  await attempt('relay', async () => { for (const socket of sockets) socket.destroy(); await new Promise((resolve, reject) => relay.close(error => error ? reject(error) : resolve())); });
  await attempt('roots', async () => { for (const root of roots) { await rm(root, { recursive: true, force: true }); await assert.rejects(stat(root), { code: 'ENOENT' }); } });
  await logs; await record('cleanup.json', cleanup);
}
if (failure) throw failure;
console.log('fair serialized drain: owned DB 50, cross-page, control interleaving and shutdown/restart passed; cleanup complete');
