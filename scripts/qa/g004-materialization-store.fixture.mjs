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
import { SemanticClaim } from '../../packages/protocol/src/semantic-claim.ts';
import { SemanticResolution, SemanticReviewOutput } from '../../packages/protocol/src/materialization.ts';
import { MemoryElement } from '../../packages/protocol/src/element.ts';
import { MemoryLink } from '../../packages/protocol/src/link.ts';
import { canonicalExtractionBody, extractionBodyDigest } from '../../packages/protocol/src/extraction.ts';

const uuid = () => '01900000-0000-7000-8000-' + randomBytes(6).toString('hex');
const hash = text => createHash('sha256').update(text).digest('hex');
const context = { principal: 'installation', commit_mode: 'receipt', client_binding: uuid() };
const incarnation = hash('g004-materialization');
const text = 'Alice lives here';
const timeValue = '2026-09-01T00:00:00Z';
const options = { uri: process.env.ANAMNESIS_TEST_NEO4J_URI, user: process.env.ANAMNESIS_TEST_NEO4J_USER ?? 'neo4j', password: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD };

test('retained claim produces one Fact and immutable retry identity', async t => {
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
  const semanticVerdicts = new Map(), resolutions = new Map();
  let semanticCalls = 0;
  const server = createServer(async (req, res) => {
    try {
      const chunks = []; let bytes = 0;
      for await (const chunk of req) { bytes += chunk.length; assert.ok(bytes <= 131072); chunks.push(chunk); }
      const input = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (input.task === 'resolve_semantic' || input.task === 'review_semantic') {
        const premises = input.task === 'resolve_semantic' ? input.input : input.input.premises;
        assert.equal(premises.source.provenance.episode_digest_version, 2);
        const value = input.task === 'resolve_semantic'
          ? resolutions.get(premises.request.proposal_id) ?? { content_language: 'und', entity_resolutions: [], attribution_speakers: [], allow_no_single_locus: false }
          : semanticVerdicts.get(premises.request.proposal_id) ?? { disposition: 'retain', semantic_claim: premises.request.semantic_claim, reason: 'Source-faithful new occurrence; no Fact-to-Fact relation authorized.' };
        if (input.task === 'review_semantic') semanticCalls++;
        res.writeHead(200, { 'content-type': 'application/json' }); res.end(JSON.stringify(value)); return;
      }
      const sourceText = input.text;
      const output = input.task === 'claim'
        ? { task: 'claim', claims: [{ text: sourceText, evidence: { start: 0, end: Buffer.byteLength(sourceText), text: sourceText } }], language: 'und', modality: 'text' }
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
  const semanticRequest = async (task, input) => {
    const response = await fetch(endpoint, { method: 'POST', headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ task, input }), signal: AbortSignal.timeout(10000) });
    assert.equal(response.status, 200);
    return response.json();
  };
  const engineOptions = { ...options, objectsRoot: root,
    extractionProvider: new HttpExtractionProvider({ endpoint, model: 'qa', model_incarnation: incarnation }),
    semanticReviewProvider: { profileId: hash('independent-semantic-judge'),
      resolve: async input => SemanticResolution.parse(await semanticRequest('resolve_semantic', input)),
      review: async input => SemanticReviewOutput.parse(await semanticRequest('review_semantic', input)) } };
  engine = new Engine(engineOptions);
  const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  await engine.init(); await engine.claimWriterEpoch();
  const generation = { id: uuid(), stream: 'extraction', incarnation, state: 'catching_up', covered_ingest_seq: 0, created_at: 100, updated_at: 100 };
  await engine.store.createExtractionGeneration(generation, context);
  const source = await engine.remember({ content: text, time: { value: timeValue, precision: 'second' }, origin: { source: 'qa', session: 'qa', actor: 'qa', record: 'one' }, source_revision: 'v1', expected_previous_revision_key: null }, { metadata: { origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] }, context });
  const retained = await engine.store.semanticEpisode(source.id, context);
  assert.equal(retained.provenance.episode_digest_version, 2);
  assert.equal(retained.provenance.lineage.complete, true);
  const task = await engine.createExtractionPipeline({ id: uuid(), generation_id: generation.id, source_id: source.id }, context);
  const pipeline = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: 'qa', lease_ms: 30000 }, context);
  assert.equal(pipeline.state, 'known');
  assert.equal(pipeline.judge_attempt.state, 'succeeded');
  assert.equal(pipeline.decisions[0].disposition, 'retain');
  // This is a well-formed candidate, not caller-supplied semantic approval.
  const semanticClaim = SemanticClaim.parse({ content: text, content_language: 'und', sub_kind: 'state', modality: 'asserted', confidence: 1,
    time: { time_value: timeValue, time_utc: Date.parse(timeValue), time_precision: 'instant', resolution: 'explicit', anchor_time_utc: null },
    entities: [], subject_keys: null, predicate_text: 'lives', scope: { object_keys: [], location_keys: [], quantities: [], condition: null, attribution_speaker_keys: [] }, scope_complete: false,
    evidence: { kind: 'source_locus', quote: text, span: { start: 0, end: Buffer.byteLength(text) } } });
  const input = { operation_id: uuid(), generation_id: generation.id, source_episode_id: source.id, judge_attempt_id: pipeline.judge_attempt.id, claim_index: 0, semantic_claim: semanticClaim };
  const snapshot = () => query('MATCH (f:Element:Fact) OPTIONAL MATCH (f)-[l:DERIVED_FROM]->(e:Element:Episode) RETURN properties(f) AS fact, properties(l) AS link, e.id AS source ORDER BY f.id,l.id');
  assert.deepEqual(await snapshot(), []);
  console.log(JSON.stringify({ checkpoint: 'materialization-input-ready', source: source.id, generation: generation.id, judge: pipeline.judge_attempt.id, source_digest: retained.content_digest, candidate_valid: true, lineage_version: 2 }));
  // The lead's original missing-method RED is retained unchanged in evidence.
  // Audit success alone remains a refusal, not the semantic positive setup.
  const semanticSnapshot = async () => ({
    facts: await snapshot(),
    entities: await query('MATCH (e:Entity) RETURN properties(e) AS entity ORDER BY e.id'),
    links: await query("MATCH (a)-[l]->(b) WHERE type(l) IN ['DERIVED_FROM','MENTIONS'] RETURN a.id AS a,b.id AS b,properties(l) AS link ORDER BY l.id"),
    arcs: await query('MATCH (a:ConductingArc) RETURN properties(a) AS arc ORDER BY a.source_id,a.link_id'),
    operations: await query('MATCH (o:MaterializationOperation) RETURN properties(o) AS operation ORDER BY o.id'),
    consumptions: await query('MATCH (c:AdjudicationConsumption) RETURN properties(c) AS consumption ORDER BY c.proposal_id'),
  });
  const original = await query('MATCH (e:Episode {id:$id}) RETURN properties(e) AS episode', { id: source.id });
  const noWrite = async (action, pattern) => {
    const before = await semanticSnapshot();
    await assert.rejects(action, pattern);
    assert.deepEqual(await semanticSnapshot(), before);
  };
  await noWrite(() => engine.materializeRetainedClaim(input, context), /retained_review_unavailable/);
  const { operation_id, ...candidate } = input;
  const proposalInput = { ...candidate, proposal_id: uuid() };
  const proposal = await engine.proposeRetainedClaim(proposalInput, context);
  assert.equal(proposal.output.disposition, 'retain');
  assert.equal(proposal.premises.source_head_revision_key, retained.revision_key);
  const authorized = { ...input, proposal_id: proposalInput.proposal_id };
  await noWrite(() => engine.materializeRetainedClaim(authorized, context), /retained_review_unavailable/);
  const review = { review_id: uuid(), proposal_id: proposalInput.proposal_id, action: 'accept', reason: 'Authenticated operator accepts the independent semantic proposal.' };
  await assert.rejects(engine.reviewRetainedClaim(review, { ...context, client_binding: undefined }), /semantic_operator_binding_required/);
  await engine.reviewRetainedClaim(review, context);
  assert.deepEqual(await engine.reviewRetainedClaim(review, context), review);
  await assert.rejects(engine.reviewRetainedClaim({ ...review, action: 'reject' }, context), /semantic_review_conflict/);
  await noWrite(() => engine.materializeRetainedClaim({ ...authorized, source_episode_id: uuid() }, context), /semantic_candidate_mismatch/);
  await noWrite(() => engine.materializeRetainedClaim({ ...authorized, judge_attempt_id: uuid() }, context), /semantic_candidate_mismatch/);
  await noWrite(() => engine.materializeRetainedClaim({ ...authorized, claim_index: 1 }, context), /semantic_candidate_mismatch/);
  await noWrite(() => engine.materializeRetainedClaim({ ...authorized, generation_id: uuid() }, context), /semantic_candidate_mismatch/);
  const first = await engine.materializeRetainedClaim(authorized, context);
  assert.equal(first.created, true);
  const uuid7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
  assert.match(first.fact_id, uuid7); assert.match(first.link_id, uuid7);
  const stored = await snapshot();
  assert.equal(stored.length, 1);
  assert.equal(stored[0].source, source.id);
  assert.equal(stored[0].fact.schema, 'anamnesis.claim/1');
  assert.equal(stored[0].fact.content, text);
  const again = await engine.materializeRetainedClaim(authorized, context);
  assert.deepEqual(again, { ...first, created: false });
  assert.deepEqual(await snapshot(), stored);
  await noWrite(() => engine.materializeRetainedClaim({ ...authorized, semantic_claim: { ...semanticClaim, scope: { ...semanticClaim.scope, condition: 'changed nested body' } } }, context), /materialization_conflict/);
  assert.deepEqual(await snapshot(), stored);

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
  await noWrite(() => engine.materializeRetainedClaim({ ...authorized, operation_id: uuid() }, context), /semantic_proposal_consumed/);
  console.log(JSON.stringify({ checkpoint: 'materialization-positive', ...first, schema_decoded: true, physical_link: physical, endpoint_rows: arcs }));

  // Activation consumes only server-derived readiness, after coverage is sealed.
  for (const partition of ['episodes', 'active_extraction']) {
    await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 1 }, context);
  }
  const selectionBefore = await engine.readExtractionSelection(context);
  assert.deepEqual(selectionBefore, { generation_id: null, selector_version: 0 });

  const bound = await engine.cutoverExtractionGenerationBound({ generation_id: generation.id, expected_generation_id: null, expected_selector_version: 0 },
    { generation_id: generation.id, profile_version: 'semantic-serving-v1', coverage_generation_id: generation.id }, context);
  assert.equal(bound.generation.state, 'active');
  assert.equal(bound.receipt.generation_id, generation.id);
  assert.equal(bound.receipt.coverage_generation_id, generation.id);
  const serving = await engine.recallHybrid({ query: text, limit: 64,
    budget: { unit: 'utf8_bytes', limit: 4096 } }, context);
  const derived = serving.results.filter(item => item.kind === 'Fact');
  assert.equal(derived.length, 1);
  assert.equal(derived[0].id, first.fact_id);
  assert.equal(serving.diagnostics.pipeline, 'derived-hybrid-v1');
  assert.equal(serving.diagnostics.ppr_used, false);
  assert.equal(serving.diagnostics.identity_mode, 'exact_episode_id');
  const servingReceipt = await engine.store.getReceipt(serving.recall_id);
  assert.ok(servingReceipt?.serving);
  assert.equal(servingReceipt.serving.response.results.some(item => item.id === first.fact_id), true);
  assert.equal(servingReceipt.lineage_selection[0].element_id, first.fact_id);
  console.log(JSON.stringify({ checkpoint: 'generation-activation-derived-recall', generation: generation.id, fact_id: first.fact_id,
    selector: await engine.readExtractionSelection(context), bound_receipt: bound.receipt, serving_receipt: servingReceipt.recall_id,
    budget: serving.budget, derived: derived[0] }));

  // A fresh Store has no in-memory operation or proposal objects. Exact replay
  // reads retained authority and identities, including after a new writer epoch.
  const beforeReopen = await semanticSnapshot();
  const callsBeforeReopen = semanticCalls;
  await engine.close(); engine = new Engine(engineOptions);
  await engine.init(); await engine.claimWriterEpoch();
  assert.deepEqual(await engine.materializeRetainedClaim(authorized, context), { ...first, created: false });
  assert.deepEqual(await engine.proposeRetainedClaim(proposalInput, context), proposal);
  assert.equal(semanticCalls, callsBeforeReopen);
  assert.deepEqual(await semanticSnapshot(), beforeReopen);
  console.log(JSON.stringify({ checkpoint: 'materialization-reopen-retry', fact_id: first.fact_id, link_id: first.link_id, provider_recalled: false }));

  const admit = async (name, target = generation, claimPatch = {}) => {
    const episode = await engine.remember({ content: text, time: { value: timeValue, precision: 'second' },
      origin: { source: 'qa', session: name, actor: 'qa', record: name }, source_revision: 'v1', expected_previous_revision_key: null },
      { metadata: { origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] }, context });
    const task = await engine.createExtractionPipeline({ id: uuid(), generation_id: target.id, source_id: episode.id }, context);
    const pipeline = await engine.runExtractionPipeline({ task_id: task.id, expected_version: task.version, worker_id: 'qa', lease_ms: 30000 }, context);
    assert.equal(pipeline.judge_attempt.state, 'succeeded');
    return { proposal_id: uuid(), generation_id: target.id, source_episode_id: episode.id, judge_attempt_id: pipeline.judge_attempt.id, claim_index: 0,
      semantic_claim: { ...semanticClaim, ...claimPatch } };
  };
  const accept = async candidate => {
    const proposal = await engine.proposeRetainedClaim(candidate, context);
    await engine.reviewRetainedClaim({ review_id: uuid(), proposal_id: candidate.proposal_id, action: 'accept', reason: 'Source-faithful bounded occurrence.' }, context);
    return { ...candidate, operation_id: uuid(), semantic_claim: proposal.output.semantic_claim };
  };
  const corrected = await admit('corrected');
  const correctedClaim = { ...semanticClaim, content: 'Alice lives in this location.' };
  semanticVerdicts.set(corrected.proposal_id, { disposition: 'correct', semantic_claim: correctedClaim, reason: 'Resolve the deictic location without changing the source assertion.' });
  const correctedInput = await accept(corrected);
  await noWrite(() => engine.materializeRetainedClaim({ ...correctedInput, semantic_claim: semanticClaim }, context), /semantic_candidate_mismatch/);
  const correction = await engine.materializeRetainedClaim(correctedInput, context);
  assert.equal((await engine.store.getElement(correction.fact_id)).content, correctedClaim.content);
  assert.notEqual((await engine.store.getElement(correction.fact_id)).content, text);
  console.log(JSON.stringify({ checkpoint: 'materialization-corrected-custody', ...correction, content: correctedClaim.content }));

  const suppressed = await admit('suppressed');
  semanticVerdicts.set(suppressed.proposal_id, { disposition: 'suppress', semantic_claim: null, reason: 'No semantic output admitted.' });
  await engine.proposeRetainedClaim(suppressed, context);
  await engine.reviewRetainedClaim({ review_id: uuid(), proposal_id: suppressed.proposal_id, action: 'accept', reason: 'Accept suppression.' }, context);
  await noWrite(() => engine.materializeRetainedClaim({ ...suppressed, operation_id: uuid() }, context), /semantic_candidate_mismatch|semantic_review_refused/);
  const rejected = await admit('rejected');
  await engine.proposeRetainedClaim(rejected, context);
  await engine.reviewRetainedClaim({ review_id: uuid(), proposal_id: rejected.proposal_id, action: 'reject', reason: 'Operator refusal.' }, context);
  await noWrite(() => engine.materializeRetainedClaim({ ...rejected, operation_id: uuid() }, context), /semantic_review_refused/);

  // The resolver, not the caller's claimed ID, supplies Entity authority.
  const inventedEntity = await admit('invented-entity', generation, { entities: [{ mention: 'Alice', entity_id: uuid() }] });
  await noWrite(() => engine.proposeRetainedClaim(inventedEntity, context), /entity_resolution_mismatch/);
  const resolvedEntity = uuid();
  const entityCandidate = await admit('resolved-entity', generation, { entities: [{ mention: 'Alice', entity_id: resolvedEntity }], subject_keys: [resolvedEntity] });
  resolutions.set(entityCandidate.proposal_id, { content_language: 'und', entity_resolutions: [{ status: 'new', mention: 'Alice', entity_id: resolvedEntity,
    normalized_name: 'Alice', entity_kind: 'person', entity_key: extractionBodyDigest({ generation: generation.id, normalized_name: 'Alice', entity_kind: 'person' }) }],
    attribution_speakers: [], allow_no_single_locus: false });
  const entityInput = await accept(entityCandidate);
  const entityFact = await engine.materializeRetainedClaim(entityInput, context);
  assert.equal(MemoryElement.parse(await engine.store.getElement(resolvedEntity)).schema, 'anamnesis.entity/1');
  const [mention] = await engine.store.linksOf(entityFact.fact_id, 'MENTIONS');
  assert.equal(MemoryLink.parse(mention).to, resolvedEntity);
  assert.equal((await query('MATCH (a:ConductingArc {link_id:$id}) RETURN a', { id: mention.id })).length, 2);

  const stale = await admit('stale-head');
  const staleInput = await accept(stale);
  const staleEpisode = await engine.store.getElement(stale.source_episode_id);
  const staleRetained = await engine.store.semanticEpisode(stale.source_episode_id, context);
  const { id: staleId, ...staleBody } = staleEpisode;
  await engine.remember({ ...staleBody, content: 'Alice moved away', source_revision: 'v2', expected_previous_revision_key: staleRetained.revision_key },
    { metadata: { origin_role: 'user', lineage_mode: 'direct', parent_recall_ids: [] }, context });
  await noWrite(() => engine.materializeRetainedClaim(staleInput, context), /stale_materialization/);

  const otherGeneration = { ...generation, id: uuid(), incarnation: hash('other-generation') };
  await engine.store.createExtractionGeneration(otherGeneration, context);
  const otherTask = await engine.createExtractionPipeline({ id: uuid(), generation_id: otherGeneration.id, source_id: source.id }, context);
  const otherPipeline = await engine.runExtractionPipeline({ task_id: otherTask.id, expected_version: otherTask.version, worker_id: 'qa', lease_ms: 30000 }, context);
  const otherInput = await accept({ ...proposalInput, proposal_id: uuid(), generation_id: otherGeneration.id, judge_attempt_id: otherPipeline.judge_attempt.id });
  const otherFact = await engine.materializeRetainedClaim(otherInput, context);
  assert.notEqual(otherFact.fact_id, first.fact_id);
  assert.equal((await query('MATCH (f:Fact {id:$id}) RETURN f.generation AS generation', { id: otherFact.fact_id }))[0].generation, otherGeneration.id);

  const aba = await admit('selector-aba', otherGeneration);
  const abaInput = await accept(aba);
  // Fault injection uses the persisted selector, never request booleans. The
  // selector returns to the same generation but its epoch must invalidate work.
  await query("MATCH (s:Meta {key:'extraction_selector'}) SET s.selector_version=s.selector_version+2");
  await noWrite(() => engine.materializeRetainedClaim(abaInput, context), /semantic_review_stale/);
  const policy = await admit('policy-stale');
  const policyInput = await accept(policy);
  const policyId = uuid();
  await engine.setPolicy({ policy_id: policyId, selector: { episode_id: policy.source_episode_id }, scope: 'content' }, context);
  await noWrite(() => engine.materializeRetainedClaim(policyInput, context), /policy_denied/);
  await engine.revokePolicy({ policy_id: policyId }, context);
  await noWrite(() => engine.materializeRetainedClaim(policyInput, context), /stale_materialization/);

  const report = await engine.verifyConductingArcs();
  assert.deepEqual(report.issues, []);
  // Keep direct assertions before handing the store to the daemon.
  assert.equal((await engine.store.verify()).filter(issue => issue.kind !== 'semantic-ineligibility').length, 0);
  const recall = await engine.recallHybrid({ query: text, limit: 64 }, context);
  assert.ok(recall.results.some(item => item.kind === 'Fact')); assert.ok(recall.results.some(item => item.kind === 'Episode'));
  assert.equal(recall.diagnostics.ppr_used, false);
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
  console.log(JSON.stringify({ checkpoint: 'materialization-refusals-and-isolation', source_mismatch: true, decision_mismatch: true,
    audit_only_refused: true, rejected_refused: true, corrected_custody: true, operation_conflict: true,
    stale_head_refused: true, selector_aba_refused: true, policy_aba_refused: true, cross_generation_distinct: true,
    conducting_verified: report, ordinary_recall: recall.diagnostics.pipeline, semantic_http_calls: semanticCalls }));
});
