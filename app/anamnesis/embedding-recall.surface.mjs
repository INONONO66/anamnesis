// Deterministic local HTTP fixture proves contracts/wiring, not semantic quality.
// Invoked only by the ownership-safe event-driven Neo4j QA runner.
import assert from 'node:assert/strict';
import { createServer } from 'node:http';
import { EventEmitter, once } from 'node:events';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, rm, stat } from 'node:fs/promises';
import { createHash, randomUUID } from 'node:crypto';
import neo4j from 'neo4j-driver';
import { RpcClient } from '../../dist/anamnesis-client.mjs';
const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
assert.ok(uri && password, 'isolated graph credentials required');
const root = await mkdtemp('/tmp/g003-hybrid-'), token = randomUUID();
const uuid = () => { const hex = randomUUID().replaceAll('-', ''), time = Date.now().toString(16).padStart(12, '0'); return `${time.slice(0,8)}-${time.slice(8)}-7${hex.slice(13,16)}-8${hex.slice(17,20)}-${hex.slice(20)}`; };
const sha256 = text => createHash('sha256').update(text).digest('hex');
// The release surface consumes the same generated projection and canonical
// serializer as the producer; no second receipt contract is maintained here.
const digestContract = process.env.RECEIPT_DIGEST_BUNDLE ? await import(process.env.RECEIPT_DIGEST_BUNDLE) : null;
assert.ok(digestContract, 'authoritative receipt digest contract required');
const { receiptBodyDigestInput, canonicalReceiptJson: canonical } = digestContract;
const profile = { model: 'deterministic-wiring-fixture-v1', model_incarnation: 'a'.repeat(64), dimensions: 3,
  document_prefix: 'document: ', query_prefix: 'query: ', max_input_bytes: 65536, norm: 'unit_l2', norm_tolerance: 0.001 };
let mode = 'ok', calls = 0;
const signals = new EventEmitter(), providerSockets = new Set();
const provider = createServer(async (req, res) => {
  try {
    const chunks = []; for await (const chunk of req) chunks.push(chunk);
    const request = JSON.parse(Buffer.concat(chunks)); calls++;
    assert.equal(request.model, profile.model); assert.equal(request.model_incarnation, profile.model_incarnation);
    assert.equal(request.dimensions, 3); assert.equal(request.truncate, false);
    signals.emit('request', request);
    if (mode === 'hold') return;
    // A deterministic rejection (400) is terminal; a 503 would be deferred within the transient budget instead.
    if (mode === 'fail') { res.writeHead(400); res.end(); return; }
    const embedding = mode === 'dimension' ? [1, 0] : mode === 'norm' ? [0, 0, 0]
      : request.input.startsWith('query: ') || request.input.includes('needle') ? [1, 0, 0] : [0, 1, 0];
    res.setHeader('content-type', 'application/json');
    res.end(JSON.stringify({ model: mode === 'model' ? 'wrong-model' : profile.model,
      model_incarnation: mode === 'incarnation' ? 'b'.repeat(64) : profile.model_incarnation, data: [{ index: 0, embedding }] }));
  } catch (error) { res.destroy(error); }
});
provider.on('connection', socket => { providerSockets.add(socket); socket.once('close', () => providerSockets.delete(socket)); });
const providerReady = once(provider, 'listening', { signal: AbortSignal.timeout(5000) }); provider.listen(0, '127.0.0.1'); await providerReady;
const config = { endpoint: `http://127.0.0.1:${provider.address().port}/v1/embeddings`, profile, timeout_ms: 5000 };
const driver = neo4j.driver(uri, neo4j.auth.basic('neo4j', password), { disableLosslessIntegers: true });
const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
let daemon, client;
async function start() {
  const child = spawn(process.execPath, ['dist/anamnesis-daemon.mjs'], { env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: root,
    ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password,
    ANAMNESIS_EMBEDDING_CONFIG: JSON.stringify(config) }, stdio: ['ignore', 'pipe', 'pipe'] });
  const lines = createInterface({ input: child.stdout }), done = once(child, 'exit');
  child.stderr.on('data', chunk => process.stderr.write(chunk));
  const ready = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(Error('daemon readiness deadline')), 30000);
    lines.on('line', line => { console.log(line); if (JSON.parse(line).event === 'listening') { clearTimeout(timer); resolve(); } });
    child.once('error', reject); child.once('exit', code => { clearTimeout(timer); reject(Error(`daemon exit ${code}`)); });
  });
  daemon = { child, done, lines }; await ready; client = await RpcClient.connect(root + '/anamnesis.sock', token);
}
async function stop() {
  if (daemon && daemon.child.exitCode === null && daemon.child.signalCode === null) {
    daemon.child.kill('SIGKILL'); await daemon.done;
  }
  await client?.close(); daemon?.lines.close();
}
async function remember(record, content, session = 'small', previous = null, sourceRevision = 'v1') {
  const result = await client.request('remember', { episode: { schema: 'anamnesis.original-message/1', content,
    time: { value: '2026-09-01T00:00:00Z', precision: 'second' }, mass: 0, properties: {},
    origin: { source: root, session, actor: 'fixture', record } }, source_revision: sourceRevision, expected_previous_revision_key: previous });
  assert.equal(result.state, 'committed'); return result;
}
const log = (name, value) => console.log(JSON.stringify({ checkpoint: name, value }));
const contextRecords = result => result.context_text ? result.context_text.split('\n').map(line => JSON.parse(line)) : [];
// Exact events, never timing: a named daemon stdout event, or the provider request for one exact input.
const daemonEvent = name => new Promise((resolve, reject) => {
  const timer = setTimeout(() => { daemon.lines.off('line', onLine); reject(Error(`${name} deadline`)); }, 20000);
  const onLine = line => { if (JSON.parse(line).event === name) { clearTimeout(timer); daemon.lines.off('line', onLine); resolve(); } };
  daemon.lines.on('line', onLine);
});
const providerRequest = input => new Promise((resolve, reject) => {
  const timer = setTimeout(() => { signals.off('request', onRequest); reject(Error('provider request deadline')); }, 20000);
  const onRequest = request => { if (request.input === input) { clearTimeout(timer); signals.off('request', onRequest); resolve(); } };
  signals.on('request', onRequest);
});
// Armed before the work is triggered: resolves at the first workers_idle the daemon prints from now on at which
// `settled()` holds, so an idle printed while the trigger was still in flight is neither missed nor mistaken for the
// one that ends the intended work. Idles are checked in order; a check never overlaps the next.
const idleWhen = settled => new Promise((resolve, reject) => {
  const timer = setTimeout(() => { daemon.lines.off('line', onLine); reject(Error('workers_idle deadline')); }, 20000);
  const finish = (done, error) => { if (!done && !error) return; clearTimeout(timer); daemon.lines.off('line', onLine); error ? reject(error) : resolve(); };
  let checks = Promise.resolve();
  const onLine = line => { if (JSON.parse(line).event !== 'workers_idle') return; checks = checks.then(settled).then(done => finish(done), error => finish(false, error)); };
  daemon.lines.on('line', onLine);
});
try {
  await start();
  assert.deepEqual((await client.request('status', {})).capabilities.recall, true);
  assert.equal((await client.request('status', {})).capabilities.embeddings, true);
  // The daemon embeds every committed original on its own lane. A rejecting provider lets it quarantine both
  // originals first, so every later provider call is attributable and the explicit recovery starts from nothing.
  mode = 'fail';
  const originalsSettled = idleWhen(async () => (await query('MATCH (x:EmbeddingAttempt) RETURN count(x) AS n'))[0].n === 2);
  const originalsAsked = Promise.all([providerRequest('document: needle A\né🙂'), providerRequest('document: unrelated original')]);
  const a = await remember('a', 'needle A\né🙂'), b = await remember('b', 'unrelated original');
  await originalsAsked; await originalsSettled;
  assert.deepEqual(await query('MATCH (x:EmbeddingAttempt) RETURN x.state AS state, x.reason AS reason ORDER BY x.episode_id'),
    [{ state: 'quarantined', reason: 'provider_rejected' }, { state: 'quarantined', reason: 'provider_rejected' }]);
  assert.equal((await query('MATCH (o:Outbox) WHERE o.processed_at IS NULL RETURN o')).length, 0);
  const failedRequest = { episode_id: a.id, operation_id: uuid() };
  const failed = await client.request('embedding.recover', failedRequest);
  assert.equal(failed.state, 'quarantined'); assert.equal(failed.reason, 'provider_rejected');
  const count = calls; assert.deepEqual(await client.request('embedding.recover', failedRequest), failed); assert.equal(calls, count);
  assert.equal((await query('MATCH (v:EmbeddingVector) RETURN v')).length, 0);
  mode = 'ok'; const recovered = await client.request('embedding.recover', { ...failedRequest, operation_id: uuid() });
  assert.equal(recovered.state, 'succeeded'); assert.equal(recovered.input_revision, a.revision_key);
  assert.equal(recovered.input_digest, (await query('MATCH (e:Episode {id:$id}) RETURN e.digest AS digest', { id: a.id }))[0].digest);
  await client.request('embedding.recover', { episode_id: b.id, operation_id: uuid() });
  const validVectors = await query('MATCH (v:EmbeddingVector) RETURN properties(v) AS p ORDER BY v.key');
  for (const [failure, reason] of [['dimension','invalid_vector'], ['norm','invalid_vector'], ['model','profile_mismatch'], ['incarnation','profile_mismatch']]) {
    mode = failure;
    const attempt = await client.request('embedding.recover', { episode_id: a.id, operation_id: uuid() });
    assert.equal(attempt.state, 'quarantined'); assert.equal(attempt.reason, reason);
    assert.deepEqual(await query('MATCH (v:EmbeddingVector) RETURN properties(v) AS p ORDER BY v.key'), validVectors);
  }
  await assert.rejects(client.request('embedding.recover', { ...failedRequest, episode_id: b.id }), { code: 'idempotency_conflict' });
  mode = 'ok';
  const request = { query: 'needle', session: { source: root, session: 'small' }, limit: 2, budget: { unit: 'utf8_bytes', limit: 65536 } };
  const hybrid = await client.request('recall', request);
  assert.deepEqual(hybrid.results.map(item => item.id), [a.id, b.id]);
  assert.deepEqual(hybrid.results[0].channels, ['bm25', 'session', 'vector']);
  assert.deepEqual(contextRecords(hybrid), hybrid.results);
  assert.equal(hybrid.used_budget, Buffer.byteLength(hybrid.context_text));
  const storedReceipt = (await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body', { id: hybrid.recall_id }))[0];
  assert.ok(storedReceipt, 'durable receipt exists before response consumption');
  const receipt = JSON.parse(storedReceipt.body);
  assert.deepEqual(receipt.primary_ids, [a.id,b.id]); assert.deepEqual(receipt.serving.response, hybrid);
  assert.deepEqual(receipt.primaries.map(item => item.sources), [[a.id],[b.id]]);
  assert.equal(receipt.serving.response.diagnostics.embedding_profile_id, recovered.profile_id);
  assert.equal(receipt.serving.context_digest, sha256(hybrid.context_text));
  assert.equal(receipt.serving.result_digest, sha256(canonical({ results: hybrid.results, companions: hybrid.companions })));
  const persistedSelection = receipt.lineage_selection ?? receipt.primaries.map(item => ({ element_id: item.id, root_episode_ids: item.sources, echo_depth: 0, complete: true }));
  const selectionBytes = canonical(persistedSelection);
  assert.equal(receipt.selection_digest, sha256(selectionBytes));
  const bodyBytes = canonical(receiptBodyDigestInput(receipt));
  assert.equal(receipt.body_digest, sha256(bodyBytes));
  assert.equal(canonical(JSON.parse(JSON.stringify(receipt))), canonical(receipt));
  assert.equal(canonical(receiptBodyDigestInput(JSON.parse(JSON.stringify(receipt)))), bodyBytes);
  log('receipt-canonical-bytes', { selection_bytes: selectionBytes, body_bytes: bodyBytes, selection_hex: Buffer.from(selectionBytes).toString('hex'), body_hex: Buffer.from(bodyBytes).toString('hex') });
  log('receipt-hashes', { body_digest: receipt.body_digest, selection_digest: receipt.selection_digest,
    context_digest: receipt.serving.context_digest, result_digest: receipt.serving.result_digest, input_digest: recovered.input_digest });
  for (const unit of ['utf8_bytes', 'unicode_scalars']) {
    const limit = unit === 'utf8_bytes' ? Buffer.byteLength(hybrid.context_text) : [...hybrid.context_text].length;
    const exact = await client.request('recall', { ...request, budget: { unit, limit } });
    assert.equal(exact.used_budget, limit); assert.deepEqual(exact.results, hybrid.results);
    const below = await client.request('recall', { ...request, budget: { unit, limit: limit - 1 } });
    assert.equal(below.results.length, 1); assert.equal(below.results[0].rank, 0);
    const zeroCalls = calls;
    const zero = await client.request('recall', { ...request, budget: { unit, limit: 0 } });
    assert.deepEqual([zero.results,zero.companions,zero.context_text,zero.used_budget], [[],[],'',0]); assert.equal(calls, zeroCalls);
    assert.ok((await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r', { id: zero.recall_id }))[0]);
  }
  await assert.rejects(client.request('recall', { ...request, budget: { unit: 'tokens', limit: 10, tokenizer_id: 'unknown-v1@sha256:'+'f'.repeat(64) } }), { code: 'invalid_budget' });
  const identity = await client.request('recall', { query: b.id, limit: 1 });
  assert.equal(identity.results[0].id, b.id); assert.ok(identity.results[0].channels.includes('identity'));
  mode = 'fail'; const degraded = await client.request('recall', request);
  assert.equal(degraded.diagnostics.vector_reason, 'provider_rejected'); assert.ok(!degraded.diagnostics.channels_used.includes('vector'));
  assert.equal(degraded.results[0].id, a.id);
  mode = 'ok';
  const feedback = { operation_id: uuid(), recall_id: hybrid.recall_id, adopted: [a.id], reward: 0 };
  assert.equal((await client.request('commit', feedback)).applied, true);
  assert.equal((await client.request('commit', feedback)).applied, false);
  assert.equal((await query('MATCH (h:Hit {namespace:$id}) RETURN count(h) AS n', { id: hybrid.recall_id }))[0].n, 2);
  const policy = { policy_id: uuid(), selector: { episode_id: a.id }, scope: 'content' };
  await client.request('policy.set', policy);
  const denied = await client.request('recall', request); assert.ok(!denied.results.some(item => item.id === a.id));
  await assert.rejects(client.request('commit', feedback), { code: 'policy_denied' });
  await client.request('policy.revoke', { policy_id: policy.policy_id });
  // Immutable revisions retain their own vectors; superseded input never serves as primary.
  const a2 = await remember('a', 'needle revised A\né🙂', 'small', a.revision_key, 'v2');
  await client.request('embedding.recover', { episode_id: a2.id, operation_id: uuid() });
  const revised = await client.request('recall', request);
  assert.equal(revised.results[0].id, a2.id); assert.equal(revised.results[0].provenance.supersedes[0].id, a.id);
  await client.request('policy.set', { ...policy, policy_id: uuid() });
  const redacted = await client.request('recall', request);
  assert.equal(redacted.results[0].provenance.supersedes_redacted, true);
  assert.deepEqual(redacted.results[0].provenance.supersedes, []);
  assert.equal(redacted.results[0].provenance.warnings[0].code, 'supersedes_withheld');
  assert.ok(redacted.context_text.includes('supersedes_withheld'));
  // Nine 63k originals fit input caps, but their duplicated structured/context
  // output cannot all fit the real RPC cap. Whole bundles are rejected.
  for (let i = 0; i < 9; i++) await remember('large-'+i, 'large '+ 'x'.repeat(63000), 'large');
  mode = 'fail'; // Isolate the session bundle cap from unrelated vector hits.
  const oversized = await client.request('recall', { query: '', session: { source: root, session: 'large' }, limit: 64, budget: { unit: 'utf8_bytes', limit: 1048576 } });
  assert.ok(oversized.results.length > 0 && oversized.results.length < 9);
  assert.ok(oversized.diagnostics.skipped_bundles > 0);
  assert.ok(Buffer.byteLength(JSON.stringify(oversized)) < 1048576);
  for (const item of oversized.results) assert.equal(item.content.length, 63006);
  // Exact HTTP request event follows the durable pending write. Kill there,
  // restart, and resume the same operation ID rather than creating duplicate work.
  const pendingRequest = { episode_id: b.id, operation_id: uuid() }; mode = 'hold';
  const requested = providerRequest('document: unrelated original');
  const pendingReply = client.request('embedding.recover', pendingRequest).catch(error => error);
  await requested;
  assert.equal(JSON.parse((await query('MATCH (a:EmbeddingAttempt {operation_id:$id}) RETURN a.body AS body', { id: pendingRequest.operation_id }))[0].body).state, 'pending');
  await stop(); assert.equal((await pendingReply).code, 'outcome_unknown');
  mode = 'ok'; await start();
  assert.equal((await client.request('embedding.status', { operation_id: pendingRequest.operation_id })).state, 'pending');
  assert.equal((await client.request('embedding.recover', pendingRequest)).state, 'succeeded');
  assert.equal((await query('MATCH (v:EmbeddingVector {episode_id:$id}) RETURN count(v) AS n', { id: b.id }))[0].n, 1);
  await assert.rejects(client.request('embedding.status', { operation_id: failedRequest.operation_id }), { code: 'policy_denied' });
  assert.equal(JSON.parse((await query('MATCH (a:EmbeddingAttempt {operation_id:$id}) RETURN a.body AS body', { id: failedRequest.operation_id }))[0].body).state, 'quarantined');
  assert.deepEqual(JSON.parse((await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body', { id: hybrid.recall_id }))[0].body), receipt);
  const allowedFeedback = { operation_id: uuid(), recall_id: identity.recall_id, adopted: [b.id], reward: 0 };
  assert.equal((await client.request('commit', allowedFeedback)).applied, true);
  assert.equal((await client.request('commit', allowedFeedback)).applied, false);
  assert.deepEqual((await client.request('hit-cache.verify', {})).issues, []);
  // A quarantined Episode returns to the outbox through embedding.requeue; the call itself wakes the lane.
  mode = 'fail';
  // The idle that ends the candidate's quarantine can be printed before remember's reply carries its id, so the
  // check waits for the identity instead of discarding that idle.
  let candidateId; const candidateKnown = new Promise(resolve => { candidateId = resolve; });
  const candidateSettled = idleWhen(async () => (await query('MATCH (a:EmbeddingAttempt {episode_id:$id}) RETURN count(a) AS n', { id: await candidateKnown }))[0].n === 1);
  const candidateAsked = providerRequest('document: requeue candidate');
  const c = await remember('c', 'requeue candidate', 'small');
  candidateId(c.id);
  await candidateAsked; await candidateSettled;
  assert.deepEqual(await query('MATCH (a:EmbeddingAttempt {episode_id:$id}) RETURN a.state AS state, a.reason AS reason', { id: c.id }), [{ state: 'quarantined', reason: 'provider_rejected' }]);
  assert.equal((await query('MATCH (v:EmbeddingVector {episode_id:$id}) RETURN count(v) AS n', { id: c.id }))[0].n, 0);
  mode = 'ok';
  const idleAfterRequeue = daemonEvent('workers_idle'), candidateRetried = providerRequest('document: requeue candidate');
  const requeued = await client.request('embedding.requeue', { limit: 100 });
  assert.ok(requeued.requeued >= 1, `requeued ${requeued.requeued}`);
  await candidateRetried; await idleAfterRequeue;
  assert.equal((await query('MATCH (v:EmbeddingVector {episode_id:$id}) RETURN count(v) AS n', { id: c.id }))[0].n, 1);
  assert.deepEqual((await query('MATCH (a:EmbeddingAttempt {episode_id:$id}) RETURN a.state AS state ORDER BY a.operation_id', { id: c.id })).map(row => row.state), ['quarantined', 'succeeded']);
  assert.deepEqual(await client.request('embedding.requeue', { limit: 100 }), { requeued: 0 });
  log('verified', { configured_provider: config.profile, recovery: ['quarantine','retry-idempotence','pending-restart','requeue'],
    hybrid_ids: hybrid.results.map(item => item.id), byte_budget: hybrid.used_budget, scalar_budget: [...hybrid.context_text].length,
    oversized_included: oversized.results.length, oversized_skipped: oversized.diagnostics.skipped_bundles,
    receipt_persisted: true, feedback_duplicate_noop: true, semantic_quality_claimed: false });
  await client.request('shutdown', {}); assert.equal((await daemon.done)[0], 0);
} finally {
  await stop(); for (const socket of providerSockets) socket.destroy();
  const closed = once(provider, 'close', { signal: AbortSignal.timeout(5000) }); provider.close(); await closed;
  await driver.close(); await rm(root, { recursive: true }); await assert.rejects(stat(root), { code: 'ENOENT' });
  log('cleanup', { daemon: 'exited', provider: 'closed', sockets: 'closed', root: 'removed', graph: 'runner-owned cleanup follows' });
}
