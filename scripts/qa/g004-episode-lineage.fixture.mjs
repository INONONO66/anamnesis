import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { connect } from 'node:net';
import neo4j from 'neo4j-driver';
import { Engine } from '../../packages/core/src/engine.ts';
import { canonicalExtractionBody } from '../../packages/protocol/src/extraction.ts';
import { validateSemanticClaim } from '../../packages/protocol/src/semantic-claim.ts';
const uuid = () => `01900000-0000-7000-8000-${randomBytes(6).toString('hex')}`;
const hash = text => createHash('sha256').update(text).digest('hex');
const context = { principal: 'installation', commit_mode: 'receipt' };
const options = { uri: process.env.ANAMNESIS_TEST_NEO4J_URI, password: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD };
const ok = reply => { assert.ok(reply.result, JSON.stringify(reply)); return reply.result; };
function peer(path) {
  const socket = connect(path), pending = new Map();
  let buffer = Buffer.alloc(0), id = 0;
  const rejectAll = error => { for (const item of pending.values()) item.reject(error); pending.clear(); };
  socket.on('error', rejectAll);
  socket.on('close', () => rejectAll(new Error('socket closed')));
  socket.on('data', bytes => {
    buffer = Buffer.concat([buffer, bytes]);
    while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
      const size = buffer.readUInt32BE(), reply = JSON.parse(buffer.subarray(4, 4 + size));
      buffer = buffer.subarray(4 + size);
      // Decode failures deliberately use id:null. This fixture sends one
      // request at a time, so that response has exactly one possible owner.
      const key = reply.id === null && pending.size === 1 ? pending.keys().next().value : reply.id;
      const waiter = pending.get(key); pending.delete(key); waiter?.resolve(reply);
    }
  });
  return { socket, request(method, params = {}) {
    return new Promise((resolve, reject) => {
      if (socket.destroyed) { reject(new Error('socket closed')); return; }
      const current = ++id;
      const timer = setTimeout(() => { pending.delete(current); reject(new Error('RPC deadline')); }, 10000);
      pending.set(current, { resolve: value => { clearTimeout(timer); resolve(value); }, reject: error => { clearTimeout(timer); reject(error); } });
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: current, method, params }));
      const header = Buffer.alloc(4); header.writeUInt32BE(body.length); socket.write(Buffer.concat([header, body]));
    });
  } };
}
async function daemon(root, fixedNow) {
  const child = spawn('node', [...(fixedNow === undefined ? [] : ['--import', 'data:text/javascript,Date.now=()=>'+fixedNow]), process.env.G004_LINEAGE_DAEMON], { env: { ...process.env,
    ANAMNESIS_NEO4J_URI: options.uri, ANAMNESIS_NEO4J_PASSWORD: options.password,
    ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: 'lineage-owned-token' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const exited = new Promise((resolve, reject) => { child.once('exit', (code, signal) => resolve({ code, signal })); child.once('error', reject); });
  child.stderr.on('data', bytes => process.stderr.write(bytes));
  const lines = createInterface({ input: child.stdout });
  const ready = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('daemon readiness deadline')), 20000);
    lines.on('line', line => { if (JSON.parse(line).event === 'listening') { clearTimeout(timer); resolve(); } });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', code => { clearTimeout(timer); reject(new Error(`daemon exited before readiness: ${code}`)); });
  });
  let p;
  const close = async () => {
    try {
      if (child.exitCode === null && child.signalCode === null) {
        const timer = setTimeout(() => child.kill('SIGKILL'), 10000);
        try { if (p && !p.socket.destroyed) await p.request('shutdown'); else child.kill('SIGTERM'); }
        finally { await exited; clearTimeout(timer); }
      }
    } finally { p?.socket.destroy(); lines.close(); }
  };
  try {
    await ready; p = peer(root + '/anamnesis.sock');
    const unauthenticated = await p.request('status');
    assert.equal(unauthenticated.error.data.code, 'unauthenticated');
    const hello = ok(await p.request('hello', { version: 1, client: 'lineage-qa', token: 'lineage-owned-token', commit_mode: 'receipt' }));
    assert.equal(hello.capabilities.extraction, false);
    console.log(JSON.stringify({ checkpoint: 'authenticated-node-uds', pid: child.pid, hello }));
    return { ...p, close };
  } catch (error) { await close(); throw error; }
}

test('bounded lineage contract boundary, independent identities and unchanged legacy behavior', async t => {
  const root = await mkdtemp('/tmp/g004-lineage-');
  const driver = neo4j.driver(options.uri, neo4j.auth.basic('neo4j', options.password), { disableLosslessIntegers: true });
  const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  let now = Date.now() - 10000;
  const engine = new Engine({ ...options, objectsRoot: root + '/objects', clock: () => now });
  const params = record => ({ episode: { schema: 'anamnesis.original-message/1', content: 'lineage owned source ' + record,
    properties: { z: 1, a: 2 }, mass: 0.5, time: { value: '2026-09-01T00:00:00Z', precision: 'second' },
    origin: { source: root, session: root, actor: 'assistant', record } }, source_revision: record, expected_previous_revision_key: null });
  const coreInput = p => ({ ...p.episode, source_revision: p.source_revision, expected_previous_revision_key: p.expected_previous_revision_key });
  const nodes = () => query('MATCH (n) RETURN elementId(n) AS identity,labels(n) AS labels,properties(n) AS props ORDER BY identity');
  const edges = () => query('MATCH (a)-[r]->(b) RETURN elementId(r) AS identity,elementId(a) AS a,elementId(b) AS b,type(r) AS type,properties(r) AS props ORDER BY identity');
  const snapshot = async () => ({ nodes: await nodes(), edges: await edges() });
  let runtime;
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const legacyParams = params('historical-valid-time'), legacy = await engine.remember(coreInput(legacyParams));
    const legacyBody = { schema: legacyParams.episode.schema, content: legacyParams.episode.content, properties: legacyParams.episode.properties,
      time: legacyParams.episode.time, payload_hash: null, previous_revision_key: null };
    // Explicit reconstructed insertion-ordered storage fixture, not a migration.
    await query('MATCH (e:Episode {id:$id}) SET e.digest=$digest REMOVE e.digest_format', { id: legacy.id, digest: hash(JSON.stringify(legacyBody)) });
    const receipt = await engine.issueReceipt({ recall_id: uuid(), primary_ids: [legacy.id], receipt_ttl_ms: 1 }, context);
    now = receipt.expires_at;
    await assert.rejects(engine.commitReceipt({ operation_id: uuid(), recall_id: receipt.recall_id, adopted: [legacy.id] }, context), /receipt_expired/);
    const beforeLegacyRetry = await snapshot();
    assert.deepEqual(await engine.remember(coreInput(legacyParams)), { id: legacy.id, created: false });
    assert.deepEqual(await snapshot(), beforeLegacyRetry);
    runtime = await daemon(root);
    const canonicalParams = params('canonical-current');
    const canonical = ok(await runtime.request('remember', canonicalParams));
    const canonicalRow = (await query('MATCH (e:Episode {id:$id}) RETURN properties(e) AS e', { id: canonical.id }))[0].e;
    const canonicalBody = { schema: canonicalParams.episode.schema, content: canonicalParams.episode.content, properties: canonicalParams.episode.properties,
      time: canonicalParams.episode.time, payload_hash: null, previous_revision_key: null };
    assert.equal(canonicalRow.digest_format, 'rfc8785-v1');
    assert.equal(canonicalRow.digest, hash(canonicalExtractionBody(canonicalBody)));
    assert.notEqual(canonicalRow.digest, hash(JSON.stringify(canonicalBody)));
    assert.equal(canonicalRow.episode_digest_version, undefined);
    const recall = ok(await runtime.request('recall', { query: '', session: { source: root, session: root }, limit: 64 }));
    ok(await runtime.request('status')); // Serial barrier includes the publication audit.
    assert.ok(recall.results.some(row => row.id === canonical.id));
    const retainedReceipt = (await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body', { id: recall.recall_id }))[0];
    console.log(JSON.stringify({ checkpoint: 'baseline-independent-query', historical: legacy, historical_digest: hash(JSON.stringify(legacyBody)), expired_parent: receipt,
      canonical, canonical_row: canonicalRow, recall, retained_receipt: JSON.parse(retainedReceipt.body), topology: await query('MATCH (a:Episode)-[:NEXT_EPISODE]->(b:Episode) RETURN a.id AS from,b.id AS to ORDER BY from,to') }));
    await runtime.close(); runtime = await daemon(root);
    const beforeRestartRetry = await snapshot();
    assert.equal(ok(await runtime.request('remember', canonicalParams)).created, false);
    assert.deepEqual(await snapshot(), beforeRestartRetry);
    console.log(JSON.stringify({ checkpoint: 'restart-exact-retry', id: canonical.id, bytes_and_identities_unchanged: true }));

    await t.test('RED: old insertion-ordered retry ignores newly supplied expired-parent lineage', async () => {
      const before = await snapshot();
      const reply = await runtime.request('remember', { ...legacyParams, origin_role: 'assistant', lineage_mode: 'receipts', parent_recall_ids: [receipt.recall_id] });
      console.log(JSON.stringify({ checkpoint: 'legacy-expired-parent-retry', reply, graph_unchanged: JSON.stringify(before) === JSON.stringify(await snapshot()) }));
      assert.equal(ok(reply).id, legacy.id);
    });
    await t.test('RED: canonical-current retry ignores newly supplied lineage before delivery binding', async () => {
      const reply = await runtime.request('remember', { ...canonicalParams, origin_role: 'assistant', lineage_mode: 'receipts', parent_recall_ids: [receipt.recall_id] });
      console.log(JSON.stringify({ checkpoint: 'canonical-retry-lineage', reply }));
      assert.equal(ok(reply).id, canonical.id);
    });
    await t.test('RED: new authenticated direct revision has digest version 2 and one lineage row', async () => {
      const reply = await runtime.request('remember', { ...params('new-direct'), origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] });
      console.log(JSON.stringify({ checkpoint: 'new-direct-admission', reply }));
      const saved = ok(reply);
      const rows = await query('MATCH (e:Episode {id:$id}) OPTIONAL MATCH (l:EchoLineage {episode_id:e.id}) RETURN e.episode_digest_version AS version,count(l) AS lineage_count', { id: saved.id });
      assert.deepEqual(rows, [{ version: 2, lineage_count: 1 }]);
    });
    await t.test('RED: admitted compatibility import cannot claim direct or satisfy all-new-v2', async () => {
      const rows = await query('MATCH (e:Episode {id:$id}) OPTIONAL MATCH (l:EchoLineage {episode_id:e.id}) RETURN properties(e) AS e,count(l) AS lineage_count', { id: canonical.id });
      console.log(JSON.stringify({ checkpoint: 'compatibility-contract-conflict', rows, source_metadata_supplied: false }));
      assert.equal(rows[0].e.episode_digest_version, undefined);
      assert.equal(rows[0].lineage_count, 0);
    });
    const direct = record => ({ ...params(record), origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] });
    const linked = (record, parents) => ({ ...params(record), origin_role: 'assistant', lineage_mode: 'receipts', parent_recall_ids: parents });
    const row = async id => (await query('MATCH (e:Episode {id:$id}) MATCH (l:EchoLineage {episode_id:$id}) RETURN properties(e) AS e,properties(l) AS l', { id }))[0];
    const receiptRow = async id => JSON.parse((await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body', { id }))[0].body);
    const remember = async p => ok(await runtime.request('remember', p));
    const recallOne = async id => {
      const result = ok(await runtime.request('recall', { query: id, limit: 1 }));
      assert.deepEqual(result.results.map(item => item.id), [id]);
      ok(await runtime.request('status'));
      assert.equal((await query('MATCH (r:RecallTransport {recall_id:$id}) RETURN r.state AS state', { id: result.recall_id }))[0].state, 'local_complete');
      return result;
    };
    const reject = async (p, code, client = runtime) => {
      const before = await snapshot(), reply = await client.request('remember', p);
      assert.equal(reply.error?.data.code, code, JSON.stringify(reply));
      assert.deepEqual(await snapshot(), before);
      console.log(JSON.stringify({ checkpoint: code, reply, graph_unchanged: true }));
    };
    await reject({ ...params('missing-lineage-schema'), origin_role: 'assistant' }, 'invalid_params');
    const badWire = await runtime.request('remember', { ...direct('bad-wire'), episode_digest_version: 2 });
    assert.equal(badWire.id, null); assert.equal(badWire.error.data.code, 'invalid_params');
    await reject(linked('unknown-parent', [uuid()]), 'unknown_recall');
    await reject(linked('parent-cap-overflow', Array.from({ length: 5 }, uuid)), 'invalid_params');
    await reject(linked('duplicate-parent', [receipt.recall_id, receipt.recall_id]), 'invalid_params');
    await reject(linked('unbound-legacy-parent', [receipt.recall_id]), 'lineage_binding_mismatch');
    for (const old of [legacyParams, canonicalParams]) {
      const before = await snapshot();
      assert.equal((await remember({ ...old, origin_role: null, lineage_mode: 'nonsense', parent_recall_ids: 17 })).created, false);
      assert.deepEqual(await snapshot(), before);
    }
    await reject({ ...legacyParams, episode: { ...legacyParams.episode, properties: { a: 2, z: 1 } } }, 'revision_conflict');
    const promotedParams = { ...direct('canonical-current'), source_revision: 'promoted-v2', expected_previous_revision_key: canonical.revision_key };
    const promoted = await remember(promotedParams);
    assert.equal((await row(promoted.id)).e.episode_digest_version, 2);
    const oldSnapshot = await snapshot();
    assert.equal((await remember({ ...canonicalParams, origin_role: null, parent_recall_ids: [uuid()] })).id, canonical.id);
    assert.deepEqual(await snapshot(), oldSnapshot);
    const sourceParams = direct('root'), source = await remember(sourceParams), sourceRow = await row(source.id);
    const directBody = { episode_id: source.id, lineage_mode: 'direct', parent_recall_ids: [], context_digests: [], root_episode_ids: [source.id], echo_depth: 0, complete: true };
    assert.equal(sourceRow.l.body, canonicalExtractionBody(directBody));
    assert.equal(sourceRow.e.lineage_digest, hash(canonicalExtractionBody(directBody)));
    assert.equal(sourceRow.e.digest_format, 'episode-rfc8785-v2');
    assert.equal(sourceRow.e.digest, hash(canonicalExtractionBody({ episode_digest_version: 2,
      schema: sourceParams.episode.schema, content: sourceParams.episode.content, properties: sourceParams.episode.properties,
      time: sourceParams.episode.time, payload_hash: null, previous_revision_key: null, origin_role: 'user', lineage_digest: sourceRow.e.lineage_digest })));
    const parent = await recallOne(source.id), parentBody = await receiptRow(parent.recall_id);
    assert.equal(parentBody.selection_digest, hash(canonicalExtractionBody(parentBody.lineage_selection)));
    assert.deepEqual(parentBody.lineage_selection, [{ element_id: source.id, root_episode_ids: [source.id], echo_depth: 0, complete: true }]);
    const childParams = linked('child', [parent.recall_id]), child = await remember(childParams), childRow = await row(child.id);
    assert.equal(ok(await runtime.request('ingest.status', { revision_key: child.revision_key, body_digest: child.body_digest, data_incarnation: child.data_incarnation })).id, child.id);
    assert.deepEqual(JSON.parse(childRow.l.body), { episode_id: child.id, lineage_mode: 'receipts', parent_recall_ids: [parent.recall_id],
      context_digests: [parentBody.selection_digest], root_episode_ids: [source.id], echo_depth: 1, complete: true });
    const foreign = peer(root + '/anamnesis.sock');
    try {
      ok(await foreign.request('hello', { version: 1, client: 'lineage-qa', token: 'lineage-owned-token', commit_mode: 'receipt' }));
      await reject(linked('foreign', [parent.recall_id]), 'lineage_binding_mismatch', foreign);
    } finally { foreign.socket.destroy(); }
    await reject({ ...childParams, origin_role: 'user' }, 'revision_conflict');
    await reject({ ...childParams, parent_recall_ids: [uuid()] }, 'revision_conflict');
    await reject({ ...childParams, episode: { ...childParams.episode, content: 'changed' } }, 'revision_conflict');
    // Legacy originals remain readable but their receipt snapshots cannot invent independence.
    const legacyRecall = await recallOne(legacy.id);
    const unknown = await remember(linked('unknown-ancestry', [legacyRecall.recall_id]));
    assert.deepEqual((await row(unknown.id)).l.root_episode_ids, []);
    assert.equal((await row(unknown.id)).l.complete, false);
    const policy = uuid(); ok(await runtime.request('policy.set', { policy_id: policy, selector: { episode_id: source.id }, scope: 'content' }));
    await reject(linked('denied-parent', [parent.recall_id]), 'policy_denied');
    // A retained child source is allowed, but its separately materialized root is denied.
    const childRecall = await recallOne(child.id);
    await reject(linked('denied-root', [childRecall.recall_id]), 'policy_denied');
    ok(await runtime.request('policy.revoke', { policy_id: policy }));
    // Four receipts and their digests are sorted as pairs, roots are a distinct union.
    const moreParents = [parent];
    for (let i = 0; i < 3; i++) moreParents.push(await recallOne((await remember(direct('union-' + i))).id));
    const reverse = moreParents.map(p => p.recall_id).sort().reverse();
    const unionParams = linked('union4', reverse), union = await remember(unionParams), unionRow = await row(union.id);
    const sortedParents = [...reverse].sort();
    assert.deepEqual(unionRow.l.parent_recall_ids, sortedParents);
    assert.deepEqual(unionRow.l.context_digests, await Promise.all(sortedParents.map(async id => (await receiptRow(id)).selection_digest)));
    assert.equal(unionRow.l.root_episode_ids.length, 4); assert.equal(unionRow.l.complete, true);
    assert.equal((await remember({ ...unionParams, parent_recall_ids: sortedParents })).created, false);
    // Seventeen real direct roots, selected through a session recall (no synthetic snapshots).
    const capSession = root + '-cap', capRoots = [];
    for (let i = 0; i < 17; i++) {
      const p = direct('cap-' + i); p.episode.origin.session = capSession;
      capRoots.push((await remember(p)).id);
    }
    const capParent = ok(await runtime.request('recall', { query: '', session: { source: root, session: capSession }, limit: 64 }));
    ok(await runtime.request('status'));
    assert.equal(capParent.results.length, 17);
    const cap = await remember(linked('roots-overflow', [capParent.recall_id])), capRow = await row(cap.id);
    assert.deepEqual(capRow.l.root_episode_ids, capRoots.sort().slice(0, 16)); assert.equal(capRow.l.complete, false);
    let depthSource = source;
    for (let depth = 1; depth <= 9; depth++) {
      const parent = await recallOne(depthSource.id);
      depthSource = await remember(linked('depth-' + depth, [parent.recall_id]));
      const retained = await row(depthSource.id);
      assert.equal(retained.l.echo_depth, Math.min(depth, 8));
      assert.equal(retained.l.complete, depth <= 8);
      assert.deepEqual(retained.l.root_episode_ids, [source.id]);
    }
    // Current and historical retries consult retained child lineage, not receipts.
    const revisedParams = { ...childParams, source_revision: 'child-v2', expected_previous_revision_key: child.revision_key,
      episode: { ...childParams.episode, content: 'revised child' } };
    const revised = await remember(revisedParams);
    await runtime.close(); runtime = await daemon(root, Date.now() + 7200000);
    const beforeExpiredWireRetry = await snapshot();
    assert.equal((await remember(childParams)).id, child.id);
    assert.equal((await remember(revisedParams)).id, revised.id);
    assert.deepEqual(await snapshot(), beforeExpiredWireRetry);
    const expiredWire = await runtime.request('commit', { operation_id: uuid(), recall_id: parent.recall_id, adopted: [source.id] });
    assert.equal(expiredWire.error.data.code, 'receipt_expired');
    console.log(JSON.stringify({ checkpoint: 'node-uds-expired-parent-exact-current-and-historical-retries', expiredWire }));
    await query('MATCH (r:RecallReceipt {recall_id:$id}) DETACH DELETE r', { id: parent.recall_id });
    const beforeRetry = await snapshot();
    assert.equal((await remember(childParams)).id, child.id);
    assert.equal((await remember(revisedParams)).id, revised.id);
    assert.deepEqual(await snapshot(), beforeRetry);
    await reject(linked('deleted-parent', [parent.recall_id]), 'unknown_recall');
    await runtime.close(); runtime = await daemon(root);
    const afterRestart = await snapshot();
    assert.equal((await remember(childParams)).created, false); assert.equal((await remember(revisedParams)).created, false);
    assert.deepEqual(await snapshot(), afterRestart);
    assert.equal((await row(child.id)).l.body, childRow.l.body);
    console.log(JSON.stringify({ checkpoint: 'v2-restart-current-historical-parent-independent-retries', child, revised }));
    assert.deepEqual(await engine.verify(), []);
    const beforeStale = await snapshot();
    const stale = await runtime.request('remember', { ...canonicalParams, source_revision: 'stale-new' });
    assert.equal(stale.error.data.code, 'stale_revision'); assert.deepEqual(await snapshot(), beforeStale);
    await engine.claimWriterEpoch();
    const beforeFence = await snapshot();
    const fenced = await runtime.request('remember', params('stale-writer'));
    assert.equal(fenced.error.data.code, 'ownership_lost'); assert.deepEqual(await snapshot(), beforeFence);
    console.log(JSON.stringify({ checkpoint: 'baseline-rollback-and-writer-fence', stale, fenced, graph_unchanged: true }));
    await runtime.close(); runtime = undefined;
    now = Date.now();
    const custody = { ...context, client_binding: randomUUID() };
    const ttlParent = await engine.issueReceipt({ recall_id: uuid(), primary_ids: [source.id], receipt_ttl_ms: 1 }, custody);
    now = ttlParent.expires_at;
    await assert.rejects(engine.commitReceipt({ operation_id: uuid(), recall_id: ttlParent.recall_id, adopted: [source.id] }, custody), { code: 'receipt_expired' });
    const expiredInput = params('retained-expired'), admission = { metadata: { origin_role: 'assistant', lineage_mode: 'receipts', parent_recall_ids: [ttlParent.recall_id] }, context: custody };
    const expiredChild = await engine.remember(coreInput(expiredInput), admission);
    assert.equal((await row(expiredChild.id)).l.complete, true);
    const priorExpiryRetry = await snapshot();
    assert.equal((await engine.remember(coreInput(expiredInput), admission)).created, false);
    assert.deepEqual(await snapshot(), priorExpiryRetry);
    await query('MATCH (r:RecallReceipt {recall_id:$id}) DETACH DELETE r', { id: ttlParent.recall_id });
    const beforeMissingRetry = await snapshot();
    assert.equal((await engine.remember(coreInput(expiredInput), admission)).created, false);
    assert.deepEqual(await snapshot(), beforeMissingRetry);
    // This adapter reads original/lineage custody, not an RPC/model replacement.
    const beforeSource = await snapshot(), retained = await engine.store.semanticEpisode(child.id, custody);
    assert.deepEqual(retained.time, { time_value: childParams.episode.time.value, time_utc: Date.parse(childParams.episode.time.value), time_precision: 'instant' });
    assert.deepEqual(retained.provenance.lineage, JSON.parse(childRow.l.body));
    const legacySource = await engine.store.semanticEpisode(legacy.id, custody);
    assert.deepEqual(legacySource.provenance, { episode_digest_version: 1, origin_role: null, lineage: null, lineage_digest: null });
    const validateSource = source => validateSemanticClaim({ content: source.content, content_language: 'en', sub_kind: 'event', modality: 'asserted', confidence: 0.5,
      time: { ...source.time, time_precision: 'inherited', resolution: 'inherited', anchor_time_utc: source.time.time_utc },
      entities: [], subject_keys: null, predicate_text: 'states', scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] },
      scope_complete: false, evidence: { kind: 'source_locus', quote: source.content } }, {
        generation: uuid(), fact_language_policy: 'source', allow_no_single_locus: false, episode: { ...source, content_language: 'en' }, entity_resolutions: [], attribution_speakers: [],
      });
    assert.equal(validateSource(retained).semantic_writes, false);
    for (const id of [legacy.id, unknown.id, cap.id, depthSource.id]) {
      const source = await engine.store.semanticEpisode(id, custody);
      assert.throws(() => validateSource(source), { code: 'echo_lineage_unavailable' });
    }
    assert.deepEqual(await snapshot(), beforeSource);
    const minute = params('legacy-minute'); minute.episode.time.precision = 'minute';
    const minuteRow = await engine.remember(coreInput(minute));
    await assert.rejects(engine.store.semanticEpisode(minuteRow.id, custody), { name: 'ZodError' });
    const invalidTime = params('nonaligned-day'); invalidTime.episode.time = { value: '2026-09-01T12:34:56Z', precision: 'day' };
    const invalidTimeRow = await engine.remember(coreInput(invalidTime));
    await assert.rejects(engine.store.semanticEpisode(invalidTimeRow.id, custody), { name: 'ZodError' });
    const counts = await query('MATCH (e:Episode) OPTIONAL MATCH (l:EchoLineage {episode_id:e.id}) OPTIONAL MATCH (o:Outbox {element_id:e.id}) RETURN e.id AS id,e.episode_digest_version AS version,count(DISTINCT l) AS lineage,count(DISTINCT o) AS outbox');
    assert.ok(counts.every(c => c.lineage === (c.version === 2 ? 1 : 0) && c.outbox === 1));
    assert.deepEqual(await query('MATCH (n) WHERE n:Fact OR n:Entity OR n:Community OR n:Hit RETURN count(n) AS count'), [{ count: 0 }]);
    assert.deepEqual(await engine.verify(), []);
    console.log(JSON.stringify({ checkpoint: 'core-expiry-source-custody-and-atomic-counts', counts, time: retained.time, semantic_writes: false }));
  } finally {
    try { await runtime?.close(); }
    finally { await Promise.all([engine.close(), driver.close()]); await rm(root, { recursive: true, force: true }); }
    console.log(JSON.stringify({ checkpoint: 'owned-fixture-cleanup', root_removed: true }));
  }
});
