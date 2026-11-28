import test from 'node:test';
import assert from 'node:assert/strict';
import { randomBytes, createHash } from 'node:crypto';
import { mkdtemp, rm, stat } from 'node:fs/promises';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { RpcClient } from '../../app/anamnesis/client.ts';
import { createServer } from 'node:http';
import { join } from 'node:path';
import { once } from 'node:events';
import neo4j from 'neo4j-driver';
import { Engine } from '../../packages/core/src/engine.ts';
import { HttpExtractionProvider } from '../../packages/core/src/extraction.ts';
import { MemoryElement } from '../../packages/protocol/src/element.ts';
import { MemoryLink } from '../../packages/protocol/src/link.ts';
import { extractionBodyDigest } from '../../packages/protocol/src/extraction.ts';

const uuid = () => '01900000-0000-7000-8000-' + randomBytes(6).toString('hex');
const hash = text => createHash('sha256').update(text).digest('hex');
const context = { principal: 'installation', commit_mode: 'receipt', client_binding: uuid() };
const incarnation = hash('g004-materialization');
const text = 'Alice lives here';
const timeValue = '2026-09-01T00:00:00Z';
const options = { uri: process.env.ANAMNESIS_TEST_NEO4J_URI, user: process.env.ANAMNESIS_TEST_NEO4J_USER ?? 'neo4j', password: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD };

test('production extraction writes Facts that remain servable on the Node daemon', async t => {
  assert.ok(options.uri && options.password, 'owned runner credentials required');
  const root = await mkdtemp('/tmp/g004-materialization-');
  let engine;
  let daemon;
  let client;
  let lines;
  let daemonExit;
  let daemonError;
  const daemonStdout = [], daemonStderr = [];
  const driver = neo4j.driver(options.uri, neo4j.auth.basic(options.user, options.password), { disableLosslessIntegers: true });
  const server = createServer(async (req, res) => {
    try {
      const chunks = []; let bytes = 0;
      for await (const chunk of req) { bytes += chunk.length; assert.ok(bytes <= 131072); chunks.push(chunk); }
      const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      const sourceText = input.text;
      const output = input.task === 'claim'
        ? { task: 'claim', claims: [{ text: sourceText, confidence: 1, sub_kind: 'state', ...(sourceText.includes('resolved') ? { entities: [{ mention: 'Alice', normalized_name: 'Alice', entity_kind: 'person' }] } : {}), evidence: { start: 0, end: Buffer.byteLength(sourceText), text: sourceText } }], language: 'und', modality: 'text' }
        : { task: 'judge_claims', claim_body_digest: input.claim_context.body_digest, decisions: [{ claim_index: 0, disposition: 'retain', evidence: input.claim_context.claims[0].evidence }], language: 'und', modality: 'text' };
      res.writeHead(200, { 'content-type': 'application/json' });
      res.end(JSON.stringify({ model: 'qa', model_incarnation: incarnation, output }));
    } catch (error) { res.writeHead(500); res.end(String(error)); }
  });
  t.after(async () => {
    // The daemon owns files below root, so stop and reap it before removing root.
    if (client) { try { await client.request('shutdown', {}); } catch {} }
    if (client) { try { await client.close(); } catch {} }
    if (daemon && daemon.exitCode === null && !daemon.signalCode) daemon.kill('SIGTERM');
    if (daemonExit) { try { await daemonExit; } catch {} }
    lines?.close();
    try { await engine?.close(); }
    finally {
      try { await driver.close(); }
      finally {
        if (server.listening) await new Promise((resolve, reject) => { server.close(error => error ? reject(error) : resolve()); server.closeAllConnections(); });
        await rm(root, { recursive: true, force: true });
        await assert.rejects(stat(root), { code: 'ENOENT' });
        console.log(JSON.stringify({ checkpoint: 'materialization-cleanup', root, removed: true,
          daemon_exit: daemon ? { code: daemon.exitCode, signal: daemon.signalCode } : null,
          daemon_error: daemonError ? String(daemonError) : null, daemon_stdout: daemonStdout.join(''), daemon_stderr: daemonStderr.join('') }));
      }
    }
  });
  const listening = once(server, 'listening', { signal: AbortSignal.timeout(10000) });
  server.listen(0, '127.0.0.1'); await listening;
  const address = server.address();
  assert.ok(address && typeof address !== 'string');
  const endpoint = 'http://127.0.0.1:' + address.port;
  const engineOptions = { ...options, objectsRoot: root, extractionJournalPath: join(root, 'extraction-state.json'),
    materializationStatePath: join(root, 'materialization-state.json'),
    extractionProvider: new HttpExtractionProvider({ endpoint, model: 'qa', model_incarnation: incarnation }) };
  engine = new Engine(engineOptions);
  const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  await engine.init(); await engine.claimWriterEpoch();
  const generation = { id: uuid(), stream: 'extraction', incarnation, state: 'catching_up', covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  const source = await engine.remember({ content: text, time: { value: timeValue, precision: 'second' }, origin: { source: 'qa', session: 'qa', actor: 'qa', record: 'one' }, source_revision: 'v1', expected_previous_revision_key: null }, { metadata: { origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] }, context });
  const retained = await engine.store.semanticEpisode(source.id, context);
  assert.equal(retained.provenance.episode_digest_version, 2);
  assert.equal(retained.provenance.lineage.complete, true);
  const original = await query('MATCH (e:Episode {id:$id}) RETURN properties(e) AS episode', { id: source.id });
  const task = await engine.createExtractionPipeline({ id: uuid(), generation_id: generation.id, source_id: source.id }, context);
  const pipeline = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: 'qa', lease_ms: 30000 }, context);
  assert.equal(pipeline.state, 'known');
  assert.equal(pipeline.judge_attempt.state, 'succeeded');
  assert.equal(pipeline.decisions[0].disposition, 'retain');
  assert.equal(pipeline.semantic_writes, true);
  const snapshot = () => query('MATCH (f:Element:Fact)-[l:DERIVED_FROM]->(e:Element:Episode) RETURN properties(f) AS fact, properties(l) AS link, e.id AS source ORDER BY f.id,l.id');
  const [written] = await snapshot();
  const first = { fact_id: written.fact.id, link_id: written.link.id };
  const occurrence = extractionBodyDigest([generation.id, source.id]);
  const custody = await engine.store.materializationState.byOccurrence(occurrence);
  assert.deepEqual(custody.fact_ids, [first.fact_id]);
  const uuid7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  assert.match(first.fact_id, uuid7); assert.match(first.link_id, uuid7);
  const stored = await snapshot();
  assert.equal(stored.length, 1);
  assert.equal(stored[0].source, source.id);
  assert.equal(stored[0].fact.schema, 'anamnesis.claim/1');
  assert.equal(stored[0].fact.content, text);
  const decoded = MemoryElement.parse(await engine.store.getElement(first.fact_id));
  assert.equal(decoded.properties.modality, 'asserted');
  const [physical] = await engine.store.linksOf(first.fact_id, 'DERIVED_FROM');
  MemoryLink.parse(physical);
  assert.equal(physical.id, first.link_id);
  assert.equal(physical.to, source.id);
  assert.deepEqual(stored[0].link.span, [0, Buffer.byteLength(text)]);
  assert.equal(stored[0].link.generation, generation.id);
  const arcs = await query('MATCH (a:ConductingArc {link_id:$id}) RETURN properties(a) AS arc ORDER BY a.source_id', { id: first.link_id });
  assert.equal(arcs.length, 2);
  assert.deepEqual(new Set(arcs.map(row => row.arc.source_id)), new Set([first.fact_id, source.id]));
  assert.ok(arcs.every(row => row.arc.generation === generation.id));
  assert.deepEqual(await query('MATCH (e:Episode {id:$id}) RETURN properties(e) AS episode', { id: source.id }), original);
  console.log(JSON.stringify({ checkpoint: 'materialization-positive', ...first, schema_decoded: true, physical_link: physical, endpoint_rows: arcs }));

  await engine.close(); engine = new Engine(engineOptions);
  await engine.init(); await engine.claimWriterEpoch();
  const replay = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: 'qa', lease_ms: 30000 }, context);
  assert.equal(replay.state, 'known');
  assert.equal(replay.semantic_writes, true);
  assert.deepEqual(await engine.store.materializationState.byOccurrence(occurrence), custody);
  assert.deepEqual(await snapshot(), stored);

  // Activation consumes only server-derived readiness, after coverage is sealed.
  for (const partition of ['episodes', 'active_extraction']) {
    await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
  }
  assert.deepEqual(await engine.store.readExtractionPipeline(task.id, context), { state: 'unknown', pipeline_id: task.id });
  assert.equal(await engine.store.materializationState.byOccurrence(occurrence), undefined);
  assert.deepEqual(await snapshot(), stored);
  const selectionBefore = await engine.readExtractionSelection(context);
  assert.deepEqual(selectionBefore, { generation_id: null, selector_version: 0 });

  const active = await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: null, expected_selector_version: 0 }, context);
  assert.equal(active.state, 'active');
  const serving = await engine.recallHybrid({ query: text, limit: 64,
    budget: { unit: 'utf8_bytes', limit: 4096 } }, context);
  const derived = serving.results.filter(item => item.kind === 'Fact');
  assert.equal(derived.length, 1);
  assert.equal(derived[0].id, first.fact_id);
  assert.equal(serving.diagnostics.pipeline, 'derived-hybrid-v1');
  // Any hit under a selected generation seeds the in-process PPR solve, so derived recall reports it.
  assert.equal(serving.diagnostics.ppr_used, true);
  assert.equal(serving.diagnostics.identity_mode, 'exact_episode_id');
  const servingReceipt = await engine.store.getReceipt(serving.recall_id);
  assert.ok(servingReceipt?.serving);
  assert.equal(servingReceipt.serving.response.results.some(item => item.id === first.fact_id), true);
  assert.equal(servingReceipt.lineage_selection[0].element_id, first.fact_id);
  console.log(JSON.stringify({ checkpoint: 'generation-activation-derived-recall', generation: generation.id, fact_id: first.fact_id,
    selector: await engine.readExtractionSelection(context), serving_receipt: servingReceipt.recall_id,
    budget: serving.budget, derived: derived[0] }));

  const otherGeneration = { ...generation, id: uuid(), incarnation: hash('other-generation') };
  await engine.store.createExtractionGeneration(otherGeneration, context);
  const otherTask = await engine.createExtractionPipeline({ id: uuid(), generation_id: otherGeneration.id, source_id: source.id }, context);
  const otherPipeline = await engine.runExtractionPipeline({ task_id: otherTask.id, expected_version: otherTask.version, worker_id: 'qa', lease_ms: 30000 }, context);
  assert.equal(otherPipeline.semantic_writes, true);
  const [otherFact] = (await engine.store.materializationState.byOccurrence(extractionBodyDigest([otherGeneration.id, source.id]))).fact_ids;
  assert.notEqual(otherFact, first.fact_id);
  assert.equal((await query('MATCH (f:Fact {id:$id}) RETURN f.generation AS generation', { id: otherFact }))[0].generation, otherGeneration.id);

  const entitySource = await engine.remember({ content: 'Alice resolved the issue', time: { value: timeValue, precision: 'second' },
    origin: { source: 'qa', session: 'qa', actor: 'qa', record: 'resolved' } },
  { metadata: { origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] }, context });
  const entityTask = await engine.createExtractionPipeline({ id: uuid(), generation_id: otherGeneration.id, source_id: entitySource.id }, context);
  const entityPipeline = await engine.runExtractionPipeline({ task_id: entityTask.id, expected_version: entityTask.version, worker_id: 'qa', lease_ms: 30000 }, context);
  assert.equal(entityPipeline.semantic_writes, true);
  const [entityFact] = (await engine.store.materializationState.byOccurrence(extractionBodyDigest([otherGeneration.id, entitySource.id]))).fact_ids;
  const [mention] = await engine.store.linksOf(entityFact, 'MENTIONS');
  assert.equal(MemoryElement.parse(await engine.store.getElement(MemoryLink.parse(mention).to)).schema, 'anamnesis.entity/1');
  assert.equal((await query('MATCH (a:ConductingArc {link_id:$id}) RETURN a', { id: mention.id })).length, 2);

  const report = await engine.verifyConductingArcs();
  assert.deepEqual(report.issues, []);
  // Keep direct assertions before handing the store to the daemon.
  assert.equal((await engine.store.verify()).filter(issue => issue.kind !== 'semantic-ineligibility').length, 0);
  const recall = await engine.recallHybrid({ query: text, limit: 64 }, context);
  assert.ok(recall.results.some(item => item.kind === 'Fact')); assert.ok(recall.results.some(item => item.kind === 'Episode'));
  assert.equal(recall.diagnostics.ppr_used, true); // seeded by the Fact and Episode hits above
  assert.equal((await engine.readExtractionSelection(context)).generation_id, generation.id);
  await engine.close(); engine = undefined;
  // Hand the persisted active generation to the real Node daemon and query over
  // its authenticated UDS; no mock client or in-process RPC substitute.
  const runtimeRoot = join(root, 'uds-runtime');
  daemon = spawn('node', [process.env.G004_DAEMON], { env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: runtimeRoot,
    ANAMNESIS_RUNTIME_TOKEN: 'g004-serving-token', ANAMNESIS_NEO4J_URI: options.uri, ANAMNESIS_NEO4J_USER: options.user, ANAMNESIS_NEO4J_PASSWORD: options.password, ANAMNESIS_NEO4J_DATABASE: 'neo4j' }, stdio: ['ignore', 'pipe', 'pipe'] });
  daemon.stdout.on('data', bytes => { daemonStdout.push(bytes.toString()); process.stdout.write(bytes); });
  daemon.stderr.on('data', bytes => { daemonStderr.push(bytes.toString()); process.stderr.write(bytes); });
  daemonExit = new Promise(resolve => { daemon.once('error', error => { daemonError = error; }); daemon.once('exit', (code, signal) => resolve({ code, signal })); });
  lines = createInterface({ input: daemon.stdout });
  const ready = new Promise((resolve, reject) => { const timer = setTimeout(() => reject(new Error('daemon readiness timeout')), 30000);
    lines.on('line', line => { try { if (JSON.parse(line).event === 'listening') { clearTimeout(timer); resolve(); } } catch {} });
    daemon.once('error', error => { clearTimeout(timer); reject(error); }); daemon.once('exit', (code, signal) => { clearTimeout(timer); reject(new Error(`daemon exit ${code ?? 'null'} signal=${signal ?? 'none'}`)); }); });
  await ready;
  client = await RpcClient.connect(runtimeRoot + '/anamnesis.sock', 'g004-serving-token', 'receipt');
  const udsRecall = await client.request('recall', { query: text, limit: 64, budget: { unit: 'utf8_bytes', limit: 4096 } });
  assert.ok(udsRecall.results.some(item => item.kind === 'Fact' && item.id === first.fact_id));
  assert.equal(udsRecall.diagnostics.pipeline, 'derived-hybrid-v1');
  await client.request('shutdown', {}); await client.close(); client = undefined;
  const exit = await daemonExit;
  assert.equal(exit.code, 0, `daemon exit ${exit.code} signal=${exit.signal}`);
  daemonExit = undefined;
  console.log(JSON.stringify({ checkpoint: 'generation-activation-derived-recall-uds', fact_id: first.fact_id, pipeline: udsRecall.diagnostics.pipeline,
    receipt_persisted: true, lineage_bound: true, budget: udsRecall.budget }));
});
