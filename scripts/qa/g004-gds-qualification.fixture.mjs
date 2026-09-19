import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { connect } from 'node:net';
import { join } from 'node:path';
import neo4j from 'neo4j-driver';
import { Engine } from '../../packages/core/src/engine.ts';
import { exportFixedCsr, solveFixedCsr } from '../../packages/core/src/dynamics/ppr.ts';
import { RpcResponse } from '../../packages/protocol/src/rpc.ts';
import { compareScores, exactReference, envelopeRefusal, FIXED20, SEEDS20, ROLE_WEIGHTS, PIN20, qualifyFixed20 } from './g004-ppr-oracle.ts';

function peer(path) {
  const socket = connect(path), waiting = new Map(); let buffer = Buffer.alloc(0), id = 0;
  const fail = error => { for (const waiter of waiting.values()) waiter.reject(error); waiting.clear(); };
  socket.on('error', fail); socket.on('close', () => fail(new Error('UDS closed')));
  socket.on('data', bytes => {
    try {
      buffer = Buffer.concat([buffer, bytes]);
      while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
        const length = buffer.readUInt32BE(), reply = RpcResponse.parse(JSON.parse(buffer.subarray(4, 4 + length)));
        buffer = buffer.subarray(4 + length); const waiter = waiting.get(reply.id);
        assert.ok(waiter, 'unexpected response ID'); waiting.delete(reply.id); waiter.resolve(reply);
      }
    } catch (error) { fail(error); socket.destroy(); }
  });
  return { socket, request(method, params = {}) {
    return new Promise((resolve, reject) => {
      const requestId = ++id;
      const timer = setTimeout(() => { waiting.delete(requestId); reject(new Error('RPC deadline')); }, 10000);
      waiting.set(requestId, { resolve: reply => { clearTimeout(timer); resolve(reply); }, reject: error => { clearTimeout(timer); reject(error); } });
      const body = Buffer.from(JSON.stringify({ jsonrpc: '2.0', id: requestId, method, params }));
      const header = Buffer.alloc(4); header.writeUInt32BE(body.length); socket.write(Buffer.concat([header, body]));
    });
  } };
}

async function udsEvidence(root, options) {
  const child = spawn('node', [process.env.G004_DAEMON], { env: { ...process.env,
    ANAMNESIS_NEO4J_URI: options.uri, ANAMNESIS_NEO4J_PASSWORD: options.password,
    ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: 'g004-oracle-fixture-token' }, stdio: ['ignore', 'pipe', 'pipe'] });
  const exited = once(child, 'exit');
  let output = '', p;
  child.stderr.on('data', bytes => { output += bytes; });
  const lines = createInterface({ input: child.stdout });
  const ready = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('daemon readiness deadline')), 30000);
    lines.on('line', line => {
      output += line + '\n';
      try { if (JSON.parse(line).event === 'listening') { clearTimeout(timer); resolve(); } }
      catch (error) { clearTimeout(timer); reject(error); }
    });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', code => { clearTimeout(timer); reject(new Error(`daemon early exit ${code}`)); });
  });
  try {
    await ready; p = peer(join(root, 'anamnesis.sock'));
    const denied = await p.request('graph.envelope', { seed_ids: [FIXED20.nodes[1]], T: PIN20.T });
    assert.equal(denied.error.data.code, 'unauthenticated');
    const hello = await p.request('hello', { version: 1, client: 'g004-qualification', token: 'g004-oracle-fixture-token', commit_mode: 'receipt' });
    assert.ok(hello.result); assert.equal(hello.result.capabilities.extraction, false);
    const envelope = await p.request('graph.envelope', { seed_ids: [FIXED20.nodes[1]], T: PIN20.T });
    // No ready coverage is published. Never feed a refused/malformed/partial
    // response into the solver. Record the actual wire error, not an invented one.
    assert.ok(envelope.error, JSON.stringify(envelope));
    const refusal = envelopeRefusal(envelope);
    const shutdown = await p.request('shutdown'); assert.equal(shutdown.result.state, 'stopping');
    const timeout = setTimeout(() => child.kill('SIGKILL'), 15000);
    const [code, signal] = await exited; clearTimeout(timeout);
    assert.equal(code, 0); assert.equal(signal, null);
    return { runtime: process.version, authenticated: true, hello, denied, envelope, qualificationRefusal: refusal, daemonExit: code };
  } finally {
    p?.socket.destroy(); lines.close();
    if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await exited; }
    await writeFile(join(process.env.G004_OUTPUT, 'daemon-output.txt'), output);
  }
}

test('real Node fixed-20 oracle, owned Neo4j capture, unavailable GDS and UDS refusal', { timeout: 120000 }, async () => {
  const options = { uri: process.env.ANAMNESIS_TEST_NEO4J_URI, password: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD };
  assert.ok(options.uri && options.password, 'owned runner credentials required');
  const root = await mkdtemp('/tmp/g004-oracle-'), runtimeRoot = await mkdtemp('/tmp/g004-oracle-uds-');
  const driver = neo4j.driver(options.uri, neo4j.auth.basic('neo4j', options.password), { disableLosslessIntegers: true });
  const engine = new Engine({ ...options, objectsRoot: root });
  let driverClosed = false, engineClosed = false;
  const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  try {
    await query('MATCH (n) DETACH DELETE n'); await engine.init(); await engine.claimWriterEpoch();
    const components = await query('CALL dbms.components() YIELD name, versions, edition RETURN name, versions, edition');
    const functions = await query("SHOW FUNCTIONS YIELD name WHERE name = 'gds.version' RETURN name");
    const procedures = await query("SHOW PROCEDURES YIELD name WHERE name = 'gds.pageRank.stream' RETURN name");
    assert.equal(functions.length, 0, 'GDS is available: use pinned 2.13.12 qualification instead of this explicitly alternate reference');
    assert.equal(procedures.length, 0);
    const gds = { available: false, requiredVersion: '2.13.12', components, functions, procedures, reason: 'gds.version and gds.pageRank.stream absent in owned Neo4j; no plugin installed or downloaded' };
    await writeFile(join(process.env.G004_OUTPUT, 'gds-availability.json'), JSON.stringify(gds, null, 2) + '\n');
    const arcRows = FIXED20.nodes.flatMap((source_id, i) => FIXED20.targets.slice(FIXED20.offsets[i], FIXED20.offsets[i + 1]).map((to, j) => ({ source_id, peer_id: FIXED20.nodes[to], link_id: `01900000-0000-7000-9000-${(FIXED20.offsets[i] + j).toString(16).padStart(12, '0')}`, role: FIXED20.roles[FIXED20.offsets[i] + j] })));
    // Disposable synthetic access metadata only, NOT physical-link authority or
    // a production completeness publication. Direct Store reads are not enabled recall.
    const context = { principal: 'installation', commit_mode: 'receipt', client_binding: 'g004-qualification' };
    const generation = { id: '01900000-0000-7000-9000-000000000500', stream: 'extraction', incarnation: 'a'.repeat(64), state: 'active', covered_ingest_seq: 0, created_at: 1, updated_at: 1 };
    await engine.store.createExtractionGeneration(generation, context);
    for (const partition of ['episodes', 'active_extraction']) await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: 0 }, context);
    await query('MERGE (s:Meta {key:"extraction_selector"}) SET s.generation_id=$id', { id: generation.id });
    await query('UNWIND $ids AS id CREATE (:Element {id:id,schema:"anamnesis.original-message/1",origin_source:"g004",time_utc:$T})', { ids: FIXED20.nodes, T: new Date(PIN20.T).toISOString() });
    await query('UNWIND $rows AS row CREATE (arc:ConductingArc) SET arc = row', { rows: arcRows });
    await query(`UNWIND $rows AS row MATCH (a:Element {id:row.source_id}), (b:Element {id:row.peer_id}) CREATE (a)-[:NEXT_EPISODE {id:row.link_id,idem_key:row.link_id}]->(b)`, { rows: arcRows });
    await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=true, m.conducting_arc_revision=1');
    const before = await query('MATCH (m:Meta {key:"meta"}) RETURN properties(m) AS meta');
    const totalRows = await query('MATCH (arc:ConductingArc) RETURN count(arc) AS count, count(CASE WHEN arc.role = "NEXT_EPISODE" THEN 1 END) AS next, count(CASE WHEN arc.role <> "NEXT_EPISODE" THEN 1 END) AS excluded');
    assert.deepEqual(totalRows, [{ count: 31, next: 6, excluded: 25 }]);
    const envelope = await engine.store.graphEnvelope(FIXED20.nodes, { T: PIN20.T, maxNodes: 20, maxArcs: 32 }, context);
    assert.equal(envelope.nodes.length, 20); assert.equal(envelope.arcs.length, 6);
    assert.equal(envelope.probes.length, 20);
    assert.equal(envelope.probes.reduce((sum, probe) => sum + probe.count, 0), 31);
    assert.ok(envelope.probes.every(probe => probe.count <= 256 && !probe.saturated));
    const expectedEligible = arcRows.filter(row => row.role === 'NEXT_EPISODE').sort((a, b) => a.link_id.localeCompare(b.link_id));
    assert.deepEqual(envelope.arcs.map(row => row.link_id), expectedEligible.map(row => row.link_id));
    assert.ok(envelope.arcs.every((row, i) => row.role === 'NEXT_EPISODE' && row.source_id === expectedEligible[i].source_id && row.peer_id === expectedEligible[i].peer_id));
    const csr = exportFixedCsr({ nodes: envelope.nodes, arcs: envelope.arcs.map(row => ({ from: row.source_id, to: row.peer_id, role: row.role, id: row.link_id })) });
    const capturedReference = exactReference(FIXED20, SEEDS20, ROLE_WEIGHTS);
    // Supplementary synthetic access-path check only; this is NOT admitted
    // online graph data, even though the same numeric operator is recovered.
    const capturedLocal = solveFixedCsr(FIXED20, SEEDS20, { roleWeights: ROLE_WEIGHTS });
    const capturedComparison = compareScores(FIXED20.nodes, capturedLocal.values, capturedReference.values,
      capturedReference.residual(capturedLocal.values), capturedReference.residual(capturedReference.values));
    assert.deepEqual(await query('MATCH (m:Meta {key:"meta"}) RETURN properties(m) AS meta'), before);
    assert.equal(envelopeRefusal(envelope), 'envelope_provenance_unavailable');
    const profile = await driver.executeQuery(`PROFILE MATCH (arc:ConductingArc {source_id:$source}) USING INDEX SEEK arc:ConductingArc(source_id,link_id)
      WHERE arc.link_id > '' RETURN arc.source_id AS source_id,arc.link_id AS link_id,arc.peer_id AS peer_id,arc.role AS role,
      arc.generation AS generation,arc.source_extraction_generation AS source_extraction_generation
      ORDER BY arc.source_id ASC,arc.link_id ASC LIMIT 256`, { source: FIXED20.nodes[6] });
    const plan = profile.summary.profile, operators = [];
    const visit = p => { operators.push({ type: p.operatorType, rows: p.rows, dbHits: p.dbHits, arguments: p.arguments }); for (const c of p.children ?? []) visit(c); };
    visit(plan);
    await writeFile(join(process.env.G004_OUTPUT, 'probe-profile.json'), JSON.stringify({ plan, operators }, null, 2) + '\n');
    const expectDegreeRefusal = async action => {
      let error;
      try { await action(); } catch (caught) { error = caught; }
      assert.ok(error, 'expected degree_probe_unavailable');
      assert.equal(error.name, 'GraphAccessError');
      assert.equal(error.code, 'degree_probe_unavailable');
      assert.equal(error.message, 'degree_probe_unavailable');
    };
    await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=false');
    await expectDegreeRefusal(() => engine.store.graphEnvelope(FIXED20.nodes, { T: PIN20.T }, context));
    await query('MATCH (m:Meta {key:"meta"}) REMOVE m.conducting_arc_ready');
    await expectDegreeRefusal(() => engine.store.probeConductingArcs(FIXED20.nodes[1], {}, context));
    // No second Bolt client while the real authority is running.
    await engine.close(); engineClosed = true; await driver.close(); driverClosed = true;
    const uds = await udsEvidence(runtimeRoot, options);
    const report = qualifyFixed20();
    const evidence = { ...report, runtime: { node: process.version, arch: process.arch, platform: process.platform }, gds,
      realCapture: { pins: PIN20, metaBefore: before, envelope, csr, operators,
        numeric: { local: { ...capturedLocal, values: [...capturedLocal.values] }, reference: capturedReference.values, ...capturedComparison },
        qualificationRefusal: envelopeRefusal(envelope), physicalAuthorityVerified: false }, uds };
    await writeFile(join(process.env.G004_OUTPUT, 'qualification.json'), JSON.stringify(evidence, null, 2) + '\n');
    console.log(JSON.stringify({ checkpoint: 'g004-qualified-independent-oracle', nodes: report.nodes, arcs: report.arcs, l1: report.l1, mass: report.local.mass, referenceResidual: report.reference.residual, topK: report.topK, gds, udsRefusal: uds.envelope }));
    // Preserve plan failures, but do not let them erase unrelated numerical or
    // refusal evidence. A saved numeric report does not imply this test passed.
    assert.ok(operators.some(p => /IndexSeek/.test(p.type)));
    assert.ok(operators.some(p => /Limit/.test(p.type)));
    assert.ok(!operators.some(p => /Sort|Top|Expand|AllNodesScan|NodeByLabelScan/.test(p.type)));
  } finally {
    if (!engineClosed) await engine.close(); if (!driverClosed) await driver.close();
    await rm(root, { recursive: true, force: true }); await rm(runtimeRoot, { recursive: true, force: true });
    console.log(JSON.stringify({ checkpoint: 'g004-oracle-cleanup', objectsRemoved: root, runtimeRemoved: runtimeRoot, driversClosed: true }));
  }
});
