import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { connect } from 'node:net';
import { createInterface } from 'node:readline';
import { mkdtemp, rm, stat } from 'node:fs/promises';
import { randomUUID, createHash } from 'node:crypto';
import neo4j from 'neo4j-driver';
const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
assert.ok(uri && password, 'owned runner credentials required');
const bundle = process.env.G003_PUBLICATION_BUNDLE ?? '.omo/evidence/g003-publication-final/green/publication-fixture.mjs';
const uuid = () => { const s = randomUUID(); return s.slice(0,14) + '7' + s.slice(15); };
const hash = text => createHash('sha256').update(text).digest('hex');
function event(emitter, name, predicate = () => true) {
  return new Promise((resolve, reject) => {
    const observe = value => { if (predicate(value)) { clearTimeout(timer); emitter.off(name, observe); resolve(value); } };
    const timer = setTimeout(() => { emitter.off(name, observe); reject(Error(`deadline: ${name}`)); }, 15000);
    emitter.on(name, observe);
  });
}
function frame(method, id, params) {
  const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', method, id, params }));
  const bytes = Buffer.alloc(body.length + 4); bytes.writeUInt32BE(body.length); body.copy(bytes, 4); return bytes;
}
async function fixture(run) {
  const root = await mkdtemp('/tmp/g003-pub-'), signals = new EventEmitter(), history = [], peers = [];
  const driver = neo4j.driver(uri, neo4j.auth.basic('neo4j', password), { disableLosslessIntegers: true });
  const query = async (text, params = {}) => (await driver.executeQuery(text, params)).records.map(row => row.toObject());
  let child, exited;
  async function start() {
    const ready = event(signals, 'listening');
    child = spawn(process.execPath, [bundle], { env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: 'publication-token', ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password }, stdio: ['ignore', 'pipe', 'pipe', 'ipc'] });
    exited = once(child, 'exit');
    const observe = value => { history.push(value); signals.emit(value.event, value); };
    child.on('message', observe);
    for (const stream of [child.stdout, child.stderr]) createInterface({ input: stream }).on('line', line => { console.log(line); observe(JSON.parse(line)); });
    await ready;
  }
  async function peer() {
    const socket = connect(root + '/anamnesis.sock'), replies = new EventEmitter(); let buffer = Buffer.alloc(0), seq = 0;
    socket.on('error', error => replies.emit('failure', error));
    socket.on('data', bytes => { buffer = Buffer.concat([buffer, bytes]); while (buffer.length >= 4 && buffer.length >= buffer.readUInt32BE() + 4) { const n = buffer.readUInt32BE() + 4, value = JSON.parse(buffer.subarray(4,n)); buffer = buffer.subarray(n); replies.emit('reply', value); } });
    const p = { socket, replies, send(method, params) { const id = ++seq; socket.write(frame(method,id,params)); return id; }, async request(method, params = {}) { const id = ++seq, response = event(replies, 'reply', value => value.id === id); socket.write(frame(method,id,params)); const value = await response; if (value.error) throw Object.assign(Error(value.error.message), value.error.data); return value.result; } };
    peers.push(p); await once(socket, 'connect', { signal: AbortSignal.timeout(15000) });
    await p.request('hello', { token: 'publication-token', client: 'publication', version: 2 }); return p;
  }
  const command = async (name, fields = {}) => { const done = event(signals,'command', value => value.name === name); child.send({ name, ...fields }); return done; };
  const remember = async (p, record, content, session = 'small', previous = null, revision = 'v1') => p.request('remember', { episode: { schema: 'anamnesis.original-message/1', content, mass: 0.5, time: { value: '2026-09-01T00:00:00Z', precision: 'second' }, properties: {}, origin: { source: root, session, actor: 'publication', record } }, source_revision: revision, expected_previous_revision_key: previous });
  const recall = (session = 'small', limit = 10, bytes = 65536) => ({ query: '', session: { source: root, session }, limit, budget: { unit: 'utf8_bytes', limit: bytes } });
  const audits = id => query('MATCH (a:RecallTransport {recall_id:$id}) RETURN properties(a) AS p', { id });
  const hits = id => query('MATCH (h:Hit {usage_id:$id}) RETURN properties(h) AS p ORDER BY h.episode_id', { id });
  const receipt = async id => (await query('MATCH (r:Usage {usage_id:$id}) RETURN r.selection AS body', { id }))[0].body;
  const originals = () => query('MATCH (e:Element:Episode {origin_source:$source}) RETURN properties(e) AS p ORDER BY e.id', { source: root });
  const caches = () => query('MATCH (c:HitCache)-[:CACHE_OF]->(e:Episode {origin_source:$source}) RETURN properties(c) AS p ORDER BY c.episode_id', { source: root });
  async function stop(control) { const stopped = event(signals,'stopped'); await control.request('shutdown'); await stopped; assert.deepEqual(await exited,[0,null]); await assert.rejects(stat(root+'/anamnesis.sock'),{code:'ENOENT'}); }
  async function crash() { child.kill('SIGKILL'); assert.deepEqual(await exited,[null,'SIGKILL']); }
  try { await start(); await run({ root, signals, history, query, peer, command, remember, recall, audits, hits, receipt, originals, caches, start, stop, crash }); }
  finally {
    for (const p of peers) p.socket.destroy();
    if (child?.exitCode === null && child?.signalCode === null) { child.kill('SIGKILL'); await exited; }
    await driver.close(); await rm(root,{recursive:true,force:true}); await assert.rejects(stat(root),{code:'ENOENT'});
    console.log(JSON.stringify({ checkpoint:'publication-cleanup',root,removed:true }));
  }
}
test('policy ack resolves real corked recall publication before accepting the new revision', { timeout:60000 }, () => fixture(async f => {
  const p = await f.peer(), control = await f.peer(); const item = await f.remember(control,'policy','policy target');
  await f.command('cork'); const queued = event(f.signals,'publication-queued'); p.send('recall',f.recall());
  const output = await queued, result = output;
  assert.ok(await f.receipt(result.usage_id),'receipt durable before corked send');
  assert.equal(output.accepted,false); assert.equal(output.writableLength,output.bytes);
  const policy = await control.request('policy.set',{policy_id:uuid(),selector:{episode_id:item.id},scope:'content'});
  const snapshot = await f.command('snapshot');
  const resolved = f.history.some(x => x.event === 'publication-closed' && x.usage_id === result.usage_id);
  await f.command('uncork'); await control.request('status');
  assert.equal(resolved,true,'policy acknowledgement preceded unresolved queued publication');
  assert.equal(snapshot.pools.general,0);
  assert.deepEqual(await f.audits(result.usage_id),[]); assert.deepEqual(await f.hits(result.usage_id),[]);
  assert.equal((await control.request('recall',f.recall())).results.length,0);
  console.log(JSON.stringify({checkpoint:'policy-publication',policy,output,resolved})); await f.stop(control);
}));
test('failed send preserves Usage and never creates a transport observation or Hit', { timeout:60000 }, () => fixture(async f => {
  const p = await f.peer(), control = await f.peer(); await f.remember(control,'failed','failed output');
  const before = await f.originals(); await f.command('cork'); const queued = event(f.signals,'publication-queued'); p.send('recall',f.recall()); const output = await queued;
  const authority = await f.receipt(output.usage_id); const closed = event(f.signals,'publication-closed'); await f.command('disconnect'); await closed; await control.request('status');
  const audits = await f.audits(output.usage_id); assert.deepEqual(audits,[]); assert.deepEqual(await f.hits(output.usage_id),[]);
  assert.equal(await f.receipt(output.usage_id),authority); assert.deepEqual(await f.originals(),before);
  assert.equal((await f.command('snapshot')).pools.general,0);
  console.log(JSON.stringify({checkpoint:'failed-send',audits,receipt_hash:hash(authority)})); await f.stop(control);
}));
test('partial frame failure stays unknown and a crash never fabricates delivery or exposure on restart', {timeout:60000}, () => fixture(async f => {
  const p=await f.peer(), control=await f.peer(); await f.remember(control,'partial','partial frame authority'); const before=await f.originals();
  await f.command('partial'); const partial=event(f.signals,'partial-output'), peerData=once(p.socket,'data',{signal:AbortSignal.timeout(15000)});
  p.send('recall',f.recall()); const output=await partial, [bytes]=await peerData;
  assert.equal(bytes.length,16); assert.ok(bytes.readUInt32BE()>12); await control.request('status');
  const audit=await f.audits(output.usage_id); assert.deepEqual(audit,[]); assert.deepEqual(await f.hits(output.usage_id),[]);
  const crashPeer=await f.peer(); await f.command('hold'); const callback=event(f.signals,'write-callback');
  const response=await crashPeer.request('recall',f.recall()); await callback;
  const authority=await f.receipt(response.usage_id); assert.deepEqual(await f.audits(response.usage_id),[]); await f.crash();
  await f.start(); const restarted=await f.peer(); await restarted.request('status');
  assert.deepEqual(await f.audits(response.usage_id),[]); assert.deepEqual(await f.hits(response.usage_id),[]);
  assert.equal(await f.receipt(response.usage_id),authority); assert.deepEqual(await f.audits(output.usage_id),audit); assert.deepEqual(await f.originals(),before);
  console.log(JSON.stringify({checkpoint:'partial-and-crash',partial:output,unknown_after_restart:response.usage_id,receipt_hash:hash(authority)})); await f.stop(restarted);
}));
test('status and shutdown remain runnable with queued publication and release reservations', {timeout:60000}, () => fixture(async f => {
  const p=await f.peer(), control=await f.peer(); await f.remember(control,'shutdown','shutdown pending output');
  await f.command('cork'); const queued=event(f.signals,'publication-queued'); p.send('recall',f.recall()); const output=await queued;
  assert.equal((await control.request('status')).storage,'available');
  assert.ok((await f.command('snapshot')).pools.general>=output.bytes);
  await f.stop(control); assert.deepEqual(await f.audits(output.usage_id),[]); assert.deepEqual(await f.hits(output.usage_id),[]);
  console.log(JSON.stringify({checkpoint:'shutdown-pending-publication',recall_id:output.usage_id}));
}));
