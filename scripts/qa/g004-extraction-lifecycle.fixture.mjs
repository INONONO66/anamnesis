import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { connect } from 'node:net';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import neo4j from 'neo4j-driver';
const uuid = () => `01900000-0000-7000-8000-${randomBytes(6).toString('hex')}`;
import { Engine } from '../../packages/core/src/engine.ts';
import { GenerationReadinessError } from '../../packages/core/src/store.ts';
import { canonicalExtractionBody, extractionBodyDigest } from '../../packages/protocol/src/extraction.ts';
import { RpcResponse } from '../../packages/protocol/src/rpc.ts';
const context = { principal: 'installation', commit_mode: 'receipt' };
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const incarnation = hash('g004-http-fixture-v1');
const options = { uri: process.env.ANAMNESIS_TEST_NEO4J_URI, password: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD };
const generation = () => ({ id: uuid(), stream: 'extraction', incarnation, state: 'active', covered_ingest_seq: 0, created_at: 100, updated_at: 100 });
async function setup() {
  const root = await mkdtemp('/tmp/g004-life-');
  let now = 100;
  const engine = new Engine({ ...options, objectsRoot: root, clock: () => now });
  const driver = neo4j.driver(options.uri, neo4j.auth.basic('neo4j', options.password), { disableLosslessIntegers: true });
  const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(r => r.toObject());
  await query('MATCH (n) DETACH DELETE n');
  await engine.init(); await engine.claimWriterEpoch();
  const source = async (content = 'Aé🙂Z') => (await engine.remember({ content, time: { value: '2026-09-01T00:00:00Z', precision: 'second' }, origin: { source: root, session: root, actor: 'qa', record: uuid() } })).id;
  return { root, engine, store: engine.store, query, source, clock: value => { now = value; }, async close() { await driver.close(); await engine.close(); await rm(root, { recursive: true, force: true }); } };
}
function claimOutput(text = 'é🙂', start = 1, end = 7) {
  const spans = [{ start, end, text }];
  const body = { task: 'claim', claims: [{ text: 'bounded fixture claim', evidence: spans[0] }], language: 'und', modality: 'text' };
  return { canonical_body: canonicalExtractionBody(body), body_digest: extractionBodyDigest(body), spans, language: 'und', modality: 'text' };
}
async function queued(f, sourceId, g = generation()) {
  await f.store.createExtractionGeneration(g, context);
  const task = await f.engine.createModelTask({ id: uuid(), generation_id: g.id, source_id: sourceId, kind: 'claim', model: 'qa-http', model_incarnation: incarnation }, context);
  return { task, g };
}
const acquire = (f, task) => f.store.leaseModelTask({ task_id: task.id, expected_version: task.version, worker_id: 'qa', lease_ms: 10 }, context);
const completion = (task, output = claimOutput()) => ({ id: task.attempt_id, task_id: task.id, expected_version: task.version, lease_epoch: task.lease.epoch, state: 'succeeded', reason: null, disposition: 'retain', spans: output.spans, output });
const snapshot = f => f.query('MATCH (n) RETURN elementId(n) AS id,labels(n) AS labels,properties(n) AS props ORDER BY id');

test('installation context is required before any extraction mutation', async () => {
  const f = await setup();
  try { const before = await snapshot(f); await assert.rejects(f.store.createExtractionGeneration(generation()), /unauthenticated/); assert.deepEqual(await snapshot(f), before); }
  finally { await f.close(); }
});
test('generation replay returns persisted record; divergent bodies conflict', async () => {
  const f = await setup();
  try { const g = generation(); assert.deepEqual(await f.store.createExtractionGeneration(g, context), g); assert.deepEqual(await f.store.createExtractionGeneration(g, context), g); await assert.rejects(f.store.createExtractionGeneration({ ...g, updated_at: 101 }, context), /generation_conflict/); }
  finally { await f.close(); }
});
test('coverage cannot create orphan generations or advance over absent terminal work', async () => {
  const f = await setup();
  try {
    // This legacy-shaped admission probe must fail even when all local bounds pass.
    await assert.rejects(f.store.recordExtractionCoverage({ generation_id: uuid(), partition: 'active_extraction', required_ingest_seq: 1, covered_ingest_seq: 1, omission_digest: hash(''), updated_at: 100 }, context));
  } finally { await f.close(); }
});
test('direct attempt admission cannot invent source revisions or persist false evidence', async () => {
  const f = await setup();
  try {
    const id = await f.source(); const g = generation(); await f.store.createExtractionGeneration(g, context);
    const forged = { id: uuid(), generation_id: g.id, source_id: id, source_revision: 'invented', body_digest: hash('wrong source'), state: 'succeeded', disposition: 'retain', created_at: 100, updated_at: 100, lease: null, output: claimOutput('xxxxxx'), policy_context: { revision: 99, authority: 'source' }, spans: [{ start: 1, end: 7, text: 'xxxxxx' }] };
    const before = await snapshot(f); await assert.rejects(f.store.recordExtractionAttempt(forged, context)); assert.deepEqual(await snapshot(f), before);
  } finally { await f.close(); }
});
test('policy changes during a leased task are revalidated before retained output', async () => {
  const f = await setup();
  try {
    const id = await f.source(); const { task } = await queued(f, id); const leased = await acquire(f, task);
    await f.engine.setPolicy({ policy_id: uuid(), selector: { episode_id: id }, scope: 'content' }, context);
    const result = await f.store.recordExtractionAttempt(completion(leased), context);
    assert.equal(result.state, 'cancelled'); assert.equal(result.reason, 'policy_denied'); assert.equal(result.output, null); assert.deepEqual(result.spans, []);
    const rows = await f.query('MATCH (a:ExtractionAttempt {id:$id}) RETURN properties(a) AS props', { id: result.id });
    assert.equal(JSON.stringify(rows).includes('bounded fixture claim'), false);
    await assert.rejects(f.store.getExtractionAttempt(result.id, context), /policy_denied/);
    await assert.rejects(f.store.createModelTask({ id: uuid(), generation_id: task.generation_id, source_id: id, kind: 'claim', model: 'qa-http', model_incarnation: incarnation }, context), /policy_denied/);
    console.log(JSON.stringify({ checkpoint: 'policy-revalidated', result }));
  } finally { await f.close(); }
});
test('immutable actual source bytes, exact output spans and attempt replay/conflict', async () => {
  const f = await setup();
  try {
    const id = await f.source(); const { task } = await queued(f, id);
    assert.equal(task.body_digest, hash('Aé🙂Z')); assert.equal(task.source_revision.length, 64);
    const leased = await acquire(f, task);
    await assert.rejects(f.store.recordExtractionAttempt(completion(leased, claimOutput('xxxxxx')), context), /span_mismatch/);
    await assert.rejects(f.store.recordExtractionAttempt(completion(leased, claimOutput('é🙂', 2, 8)), context), /span_mismatch/);
    const forged = completion(leased); forged.output.body_digest = hash('forged');
    await assert.rejects(f.store.recordExtractionAttempt(forged, context));
    await f.query('MATCH (e:Episode {id:$id}) SET e.content=$text', { id, text: 'changed bytes' });
    await assert.rejects(f.store.recordExtractionAttempt(completion(leased), context), /stale_input/);
    await f.query('MATCH (e:Episode {id:$id}) SET e.content=$text', { id, text: 'Aé🙂Z' });
    const input = completion(leased), result = await f.store.recordExtractionAttempt(input, context);
    assert.equal(result.state, 'succeeded'); assert.deepEqual(result.policy_context, { revision: 0, authority: 'installation' });
    assert.deepEqual(await f.store.recordExtractionAttempt(input, context), result);
    const changed = completion(leased, claimOutput('A', 0, 1));
    await assert.rejects(f.store.recordExtractionAttempt(changed, context), /attempt_conflict/);
    assert.deepEqual(await f.store.getExtractionAttempt(result.id, context), result);
    console.log(JSON.stringify({ checkpoint: 'immutable-attempt', result }));
  } finally { await f.close(); }
});
test('task CAS fences concurrent acquisition, expiry, cancellation, worker loss and explicit retry', async () => {
  const f = await setup();
  try {
    const { task } = await queued(f, await f.source());
    const races = await Promise.allSettled([acquire(f, task), acquire(f, task)]);
    assert.equal(races.filter(r => r.status === 'fulfilled').length, 1);
    const leased = races.find(r => r.status === 'fulfilled').value;
    await assert.rejects(f.store.settleModelTask({ task_id: task.id, expected_version: leased.version, lease_epoch: leased.lease.epoch, reason: 'expired' }, context), /lease_not_expired/);
    f.clock(110);
    await assert.rejects(f.store.recordExtractionAttempt(completion(leased), context), /lease_expired/);
    const expired = await f.store.settleModelTask({ task_id: task.id, expected_version: leased.version, lease_epoch: leased.lease.epoch, reason: 'expired' }, context);
    assert.equal(expired.state, 'expired');
    const retry = await f.store.retryModelTask({ task_id: task.id, expected_version: expired.version }, context);
    const second = await acquire(f, retry); assert.notEqual(second.attempt_id, leased.attempt_id); assert.notEqual(second.lease.epoch, leased.lease.epoch);
    await assert.rejects(f.store.recordExtractionAttempt(completion(leased), context), /attempt_conflict/);
    const cancelled = await f.store.cancelModelTask({ task_id: task.id, expected_version: second.version }, context);
    assert.equal(cancelled.state, 'cancelled');
    await assert.rejects(f.store.recordExtractionAttempt(completion(second), context), /attempt_conflict/);
    await assert.rejects(f.store.retryModelTask({ task_id: task.id, expected_version: cancelled.version }, context), /invalid_transition/);
    const other = await queued(f, await f.source()); const lost = await acquire(f, other.task);
    await assert.rejects(f.store.settleModelTask({ task_id: lost.id, expected_version: lost.version, lease_epoch: lost.lease.epoch, reason: 'worker_lost' }, context), /worker_still_owned/);
    const replacement = new Engine({ ...options, objectsRoot: f.root, clock: () => 110 });
    try {
      await replacement.claimWriterEpoch();
      await assert.rejects(f.store.recordExtractionAttempt(completion(lost), context), /stale_writer_epoch/);
      const settled = await replacement.store.settleModelTask({ task_id: lost.id, expected_version: lost.version, lease_epoch: lost.lease.epoch, reason: 'worker_lost' }, context);
      assert.equal(settled.state, 'worker_lost');
      assert.equal((await replacement.store.getExtractionAttempt(lost.attempt_id, context)).state, 'worker_lost');
      await assert.rejects(f.store.createExtractionGeneration(generation(), context), /stale_writer_epoch/);
      console.log(JSON.stringify({ checkpoint: 'lease-recovery', expired, cancelled, settled }));
    } finally { await replacement.close(); }
  } finally { await f.close(); }
});
test('cutover rejects missing coverage atomically', async () => {
  const f = await setup();
  try {
    const g = generation(); await f.store.createExtractionGeneration(g, context);
    const before = await snapshot(f);
    await assert.rejects(f.store.cutoverExtractionGeneration({ generation_id: g.id, expected_generation_id: null, expected_selector_version: 0 }, context), /coverage/);
    assert.deepEqual(await snapshot(f), before);
  } finally { await f.close(); }
});

test('coverage derives a bounded contiguous terminal prefix and updates generation atomically', async () => {
  const f = await setup();
  try {
    const g = generation(); const first = await queued(f, await f.source(), g); const second = await queued(f, await f.source(), g);
    const b = await acquire(f, second.task); await f.store.recordExtractionAttempt(completion(b), context);
    const advance = (covered, expected = 0, partition = 'active_extraction') => f.store.recordExtractionCoverage({ generation_id: g.id, partition, expected_covered_ingest_seq: expected, covered_ingest_seq: covered }, context);
    const before = await snapshot(f); await assert.rejects(advance(2), /coverage_hole/); assert.deepEqual(await snapshot(f), before);
    const a = await acquire(f, first.task); await f.store.recordExtractionAttempt(completion(a), context);
    const coverage = await advance(2); assert.equal(coverage.covered_ingest_seq, 2); assert.equal(coverage.required_ingest_seq, 2);
    assert.equal((await f.store.getExtractionGeneration(g.id, context)).covered_ingest_seq, 0);
    await advance(2, 0, 'episodes'); assert.equal((await f.store.getExtractionGeneration(g.id, context)).covered_ingest_seq, 2);
    await assert.rejects(advance(1, 2), /coverage_regression/); await assert.rejects(advance(2, 0), /coverage_conflict/);
    const third = await queued(f, await f.source(), g); const lease = await acquire(f, third.task);
    const failed = await f.store.recordExtractionAttempt({ ...completion(lease), state: 'failed', reason: 'provider_unavailable', disposition: null, output: null, spans: [] }, context);
    const omitted = await advance(3, 2); assert.notEqual(omitted.omission_digest, coverage.omission_digest);
    const terminal = await f.store.getExtractionTask(third.task.id, context);
    await assert.rejects(f.store.retryModelTask({ task_id: terminal.id, expected_version: terminal.version }, context), /coverage_frozen/);
    console.log(JSON.stringify({ checkpoint: 'contiguous-coverage', coverage, omitted, failed }));
  } finally { await f.close(); }
});

for (const path of ['request','reader','rollback']) test(`selector version fences A-era ${path} after persisted A-B-A history`, async () => {
  const f = await setup();
  try {
    const a = generation(), b = generation(), c = generation();
    for (const g of [a,b,c]) {
      await f.store.createExtractionGeneration(g, context);
      for (const partition of ['episodes','active_extraction']) await f.store.recordExtractionCoverage({generation_id:g.id,partition,expected_covered_ingest_seq:0,covered_ingest_seq:0},context);
    }
    // A retained selection history fixture, NOT a successful activation claim:
    // production cutover now refuses the unavailable derived serving proofs.
    const selectHistory = (id, version) => f.query("MATCH (m:Meta {key:'extraction_selector'}) SET m.generation_id=$id,m.selector_version=$version", {id,version});
    await selectHistory(a.id,1);
    const pinned = await f.engine.readExtractionCoverage({generation_id:a.id},context);
    const stale = {generation_id:c.id,expected_generation_id:a.id,expected_selector_version:pinned.selector_version};
    await selectHistory(b.id,2); await selectHistory(a.id,3);
    const before = await snapshot(f);
    if (path === 'request') await assert.rejects(f.engine.cutoverExtractionGeneration(stale, context), {code:'selector_version_conflict'});
    else if (path === 'rollback') await assert.rejects(f.engine.rollbackExtractionGeneration(stale,context), {code:'selector_version_conflict'});
    else await assert.rejects(f.engine.readExtractionCoverage({generation_id:a.id,expected_selector_version:pinned.selector_version},context), {code:'selector_version_conflict'});
    assert.deepEqual(await snapshot(f),before);
    assert.equal((await f.engine.readExtractionCoverage({generation_id:a.id},context)).selector_version,3);
    console.log(JSON.stringify({checkpoint:'persisted-aba-fence',path,old_version:pinned.selector_version,current_version:3,activation_supported:false}));
  } finally { await f.close(); }
});

test('cutover rejects real remembered ingest beyond explicit target coverage', async () => {
  const f = await setup();
  try {
    const g = generation(); await f.store.createExtractionGeneration(g,context);
    for (const partition of ['episodes','active_extraction']) await f.store.recordExtractionCoverage({generation_id:g.id,partition,expected_covered_ingest_seq:0,covered_ingest_seq:0},context);
    await f.source();
    const before = await snapshot(f);
    await assert.rejects(f.store.cutoverExtractionGeneration({generation_id:g.id,expected_generation_id:null,expected_selector_version:0},context),/coverage/);
    assert.deepEqual(await snapshot(f),before);
  } finally { await f.close(); }
});

test('caught-up audit coverage refuses unavailable activation proofs without mutations', async () => {
  const f = await setup();
  try {
    const g = {...generation(),state:'catching_up'}; await f.store.createExtractionGeneration(g,context);
    for (const partition of ['episodes','active_extraction']) await f.store.recordExtractionCoverage({generation_id:g.id,partition,expected_covered_ingest_seq:0,covered_ingest_seq:0},context);
    const indexes = await f.query('SHOW INDEXES YIELD name,state WHERE name IN $names RETURN name,state', { names: ['conducting_arc_source_link','conducting_arc_coverage','extraction_coverage_key','extraction_generation_id','meta_key','fact_generation_id','entity_generation_key'] });
    assert.equal(indexes.length, 7); assert.ok(indexes.every(index => index.state === 'ONLINE'));
    assert.deepEqual(await f.query('MATCH (f:Element:Fact) OPTIONAL MATCH (w:EntityWitness) OPTIONAL MATCH ()-[i:INVALIDATES]->() RETURN count(DISTINCT f) AS facts,count(DISTINCT w) AS witnesses,count(DISTINCT i) AS invalidations'), [{ facts: 0, witnesses: 0, invalidations: 0 }]);
    assert.deepEqual(await f.query('MATCH (o:MaterializationOperation) RETURN count(o) AS count'), [{ count: 0 }]);
    const before = await snapshot(f);
    await assert.rejects(f.engine.cutoverExtractionGeneration({generation_id:g.id,expected_generation_id:null,expected_selector_version:0},context), error => {
      assert.ok(error instanceof GenerationReadinessError); assert.equal(error.name,'GenerationReadinessError'); assert.equal(error.code,'activation_prerequisite_unavailable');
      assert.deepEqual(error.prerequisites,['selected_model_embedding_coverage']);
      console.log(JSON.stringify({checkpoint:'activation-fail-closed',code:error.code,prerequisites:error.prerequisites})); return true;
    });
    assert.deepEqual(await snapshot(f),before);
  } finally { await f.close(); }
});

for (const state of ['queued','leased']) test(`cutover refuses real ${state} target tasks atomically`, async () => {
  const f = await setup();
  try {
    const g = {...generation(),state:'catching_up'}; await f.store.createExtractionGeneration(g,context);
    for (const partition of ['episodes','active_extraction']) await f.store.recordExtractionCoverage({generation_id:g.id,partition,expected_covered_ingest_seq:0,covered_ingest_seq:0},context);
    const task = await f.engine.createModelTask({id:uuid(),generation_id:g.id,source_id:await f.source(),kind:'claim',model:'qa-http',model_incarnation:incarnation},context);
    if (state === 'leased') await acquire(f,task);
    const before = await snapshot(f);
    await assert.rejects(f.engine.cutoverExtractionGeneration({generation_id:g.id,expected_generation_id:null,expected_selector_version:0},context),{code:'generation_work_in_flight'});
    assert.deepEqual(await snapshot(f),before);
  } finally { await f.close(); }
});

test('rollback advances server epoch and captures live watermark; concurrent stale retry cannot mutate', async () => {
  const f = await setup();
  try {
    const a = generation(), b = generation();
    for (const g of [a,b]) await f.store.createExtractionGeneration(g,context);
    const retired = {...b,state:'retired'};
    await f.query("MATCH (g:ExtractionGeneration {id:$id}) SET g.body=$body,g.state='retired'",{id:b.id,body:canonicalExtractionBody(retired)});
    await f.query("MATCH (s:Meta {key:'extraction_selector'}) SET s.generation_id=$id",{id:a.id});
    await f.source();
    const request = {generation_id:b.id,expected_generation_id:a.id,expected_selector_version:0};
    const results = await Promise.allSettled([f.engine.rollbackExtractionGeneration(request,context),f.engine.rollbackExtractionGeneration(request,context)]);
    assert.equal(results.filter(r=>r.status==='fulfilled').length,1);
    assert.equal(results.find(r=>r.status==='rejected').reason.code,'selector_version_conflict');
    assert.deepEqual(await f.engine.readExtractionSelection(context),{generation_id:a.id,selector_version:1});
    assert.equal((await f.query('MATCH (g:ExtractionGeneration {id:$id}) RETURN g.source_high_watermark AS watermark',{id:b.id}))[0].watermark,1);
    const before = await snapshot(f);
    await assert.rejects(f.engine.rollbackExtractionGeneration(request,context),{code:'selector_version_conflict'});
    assert.deepEqual(await snapshot(f),before);
    await f.engine.init(); assert.equal((await f.engine.readExtractionSelection(context)).selector_version,1);
    console.log(JSON.stringify({checkpoint:'rollback-monotone-epoch',selector_version:1,watermark:1,concurrent_winners:1}));
  } finally { await f.close(); }
});

test('configured real HTTP Engine task, bounded failures, policy and cancellation during provider work', async () => {
  let mode = 'normal', held;
  const server = createServer(async (req, res) => {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const request = JSON.parse(Buffer.concat(chunks));
    assert.equal(request.model, 'qa-http'); assert.equal(request.model_incarnation, incarnation); assert.equal(request.text, 'Aé🙂Z');
    if (mode === 'hold') { held = res; server.emit('held'); return; }
    res.setHeader('content-type', 'application/json');
    if (mode === 'oversize') res.end(JSON.stringify({ model: 'qa-http', model_incarnation: incarnation, output: 'x'.repeat(65537) }));
    else res.end(JSON.stringify({ model: 'qa-http', model_incarnation: incarnation, output: JSON.parse(claimOutput().canonical_body) }));
  });
  const listening = once(server, 'listening', { signal: AbortSignal.timeout(10000) }); server.listen(0, '127.0.0.1'); await listening;
  const previous = process.env.ANAMNESIS_EXTRACTION_CONFIG;
  process.env.ANAMNESIS_EXTRACTION_CONFIG = JSON.stringify({ endpoint: `http://127.0.0.1:${server.address().port}`, model: 'qa-http', model_incarnation: incarnation, timeout_ms: 10000 });
  const f = await setup();
  const run = task => f.engine.runExtractionTask({ task_id: task.id, expected_version: task.version, worker_id: 'http-worker', lease_ms: 30000 }, context);
  try {
    const { task } = await queued(f, await f.source());
    const result = await run(task); assert.equal(result.state, 'succeeded');
    mode = 'oversize'; const tooBig = await queued(f, await f.source());
    const failed = await run(tooBig.task); assert.equal(failed.state, 'failed'); assert.equal(failed.reason, 'output_too_large'); assert.equal(failed.output, null);
    mode = 'hold'; const denied = await queued(f, await f.source());
    const received = once(server, 'held', { signal: AbortSignal.timeout(10000) }); const running = run(denied.task); await received;
    await f.engine.setPolicy({ policy_id: uuid(), selector: { episode_id: denied.task.source_id }, scope: 'content' }, context);
    held.end(JSON.stringify({ model: 'qa-http', model_incarnation: incarnation, output: JSON.parse(claimOutput().canonical_body) }));
    assert.equal((await running).reason, 'policy_denied');
    const cancelled = await queued(f, await f.source());
    const receivedAgain = once(server, 'held', { signal: AbortSignal.timeout(10000) });
    const late = run(cancelled.task).then(value => ({ value }), error => ({ error })); await receivedAgain;
    const leased = await f.store.getExtractionTask(cancelled.task.id, context);
    await f.store.cancelModelTask({ task_id: leased.id, expected_version: leased.version }, context);
    held.end(JSON.stringify({ model: 'qa-http', model_incarnation: incarnation, output: JSON.parse(claimOutput().canonical_body) }));
    assert.match(String((await late).error), /attempt_conflict/);
    console.log(JSON.stringify({ checkpoint: 'configured-http-worker', success: result.id, failed, policy_race: 'content-free', cancel_race: 'late output fenced' }));
  } finally {
    if (previous === undefined) delete process.env.ANAMNESIS_EXTRACTION_CONFIG; else process.env.ANAMNESIS_EXTRACTION_CONFIG = previous;
    server.closeAllConnections(); const closed = once(server, 'close'); server.close(); await closed; await f.close();
  }
});

function peer(path) {
  const socket = connect(path); let buffer = Buffer.alloc(0), id = 0;
  const waiters = [];
  socket.on('data', bytes => {
    buffer = Buffer.concat([buffer, bytes]);
    while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
      const length = buffer.readUInt32BE(), reply = RpcResponse.parse(JSON.parse(buffer.subarray(4, 4 + length)));
      buffer = buffer.subarray(4 + length); waiters.shift()?.resolve(reply);
    }
  });
  socket.on('error', error => { for (const waiter of waiters.splice(0)) waiter.reject(error); });
  socket.on('close', () => { for (const waiter of waiters.splice(0)) waiter.reject(new Error('socket closed')); });
  return { socket, request(method, params = {}) {
    return new Promise((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error('RPC deadline')), 10000);
      waiters.push({ resolve: value => { clearTimeout(timer); resolve(value); }, reject: error => { clearTimeout(timer); reject(error); } });
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: ++id, method, params }));
      const header = Buffer.alloc(4); header.writeUInt32BE(body.length); socket.write(Buffer.concat([header, body]));
    });
  } };
}
test('real Node UDS authenticates ingestion/policy, keeps extraction false, and supplies actual lifecycle Episode bytes', async () => {
  const f = await setup();
  const runtimeRoot = await mkdtemp('/tmp/g004-uds-');
  const child = spawn('node', [process.env.G004_DAEMON], { env: { ...process.env, ANAMNESIS_NEO4J_URI: options.uri, ANAMNESIS_NEO4J_PASSWORD: options.password, ANAMNESIS_RUNTIME_ROOT: runtimeRoot, ANAMNESIS_RUNTIME_TOKEN: 'g004-installation-token' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const exited = once(child, 'exit', { signal: AbortSignal.timeout(30000) });
  child.stderr.on('data', bytes => process.stderr.write(bytes));
  const lines = createInterface({ input: child.stdout });
  const ready = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('readiness deadline')), 20000);
    lines.on('line', line => { if (JSON.parse(line).event === 'listening') { clearTimeout(timer); resolve(); } });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', code => { clearTimeout(timer); reject(new Error(`daemon exited ${code}`)); });
  });
  let p;
  const ok = reply => { assert.ok('result' in reply, JSON.stringify(reply)); return reply.result; };
  try {
    await ready; p = peer(runtimeRoot + '/anamnesis.sock');
    assert.equal((await p.request('policy.set', { policy_id: uuid(), selector: { source: runtimeRoot }, scope: 'content' })).error.data.code, 'unauthenticated');
    assert.equal((await p.request('hello', { version: 1, client: 'installation', token: 'wrong', commit_mode: 'receipt' })).error.data.code, 'authentication_failed');
    const hello = ok(await p.request('hello', { version: 1, client: 'qa-not-authority', token: 'g004-installation-token', commit_mode: 'receipt' }));
    assert.equal(hello.principal, 'installation'); assert.equal(hello.capabilities.extraction, false);
    const episode = { schema: 'anamnesis.original-message/1', content: 'Aé🙂Z', mass: 1, properties: {}, time: { value: '2026-09-01T00:00:00Z', precision: 'second' }, origin: { source: runtimeRoot, session: runtimeRoot, actor: 'qa', record: 'one' } };
    const remembered = ok(await p.request('remember', { episode, source_revision: 'one', expected_previous_revision_key: null }));
    assert.equal(remembered.state, 'committed');
    const deniedEpisode = ok(await p.request('remember', { episode: { ...episode, origin: { ...episode.origin, record: 'denied' } }, source_revision: 'denied', expected_previous_revision_key: null }));
    const policy = ok(await p.request('policy.set', { policy_id: uuid(), selector: { episode_id: deniedEpisode.id }, scope: 'content' }));
    await p.request('shutdown'); assert.equal((await exited)[0], 0);
    // The daemon relinquished its writer. This is an internal Engine call, not an extraction RPC.
    await f.engine.claimWriterEpoch();
    const { task, g } = await queued(f, remembered.id); const lease = await acquire(f, task);
    const attempt = await f.store.recordExtractionAttempt(completion(lease), context);
    assert.equal(attempt.source_revision, remembered.revision_key); assert.equal(attempt.body_digest, hash(episode.content)); assert.equal(attempt.policy_context.revision, policy.policy_revision);
    await assert.rejects(f.store.createModelTask({ id: uuid(), generation_id: g.id, source_id: deniedEpisode.id, kind: 'claim', model: 'qa-http', model_incarnation: incarnation }, context), /policy_denied/);
    console.log(JSON.stringify({ checkpoint: 'real-node-uds', hello, remembered, policy, internal_attempt: attempt.id, extraction_rpc: false }));
  } finally {
    p?.socket.destroy(); lines.close();
    if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await exited; }
    await rm(runtimeRoot, { recursive: true, force: true }); await f.close();
    console.log(JSON.stringify({ checkpoint: 'uds-cleanup', process_stopped: child.exitCode !== null || child.signalCode !== null, runtime_removed: true }));
  }
});
