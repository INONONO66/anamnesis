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
const context = { principal: 'installation', commit_mode: 'receipt' };
const id = n => `01900000-0000-7000-8000-${n.toString(16).padStart(12, '0')}`;
const T = Date.parse('2026-01-01T00:00:00Z');
const output = process.env.G004_ORDERED_OUTPUT;
const save = (name, value) => writeFile(join(output, name), JSON.stringify(value, null, 2));
const plans = plan => [plan, ...(plan.children ?? []).flatMap(plans)];
function bounded(profile, count) {
  const operators = plans(profile);
  assert.ok(!operators.some(p => /Top|Sort|Scan|Expand/.test(p.operatorType)));
  const limits = operators.filter(p => /^Limit(?:@|$)/.test(p.operatorType));
  const seeks = operators.filter(p => /IndexSeek/.test(p.operatorType));
  assert.equal(limits.length, 1); assert.equal(seeks.length, 1);
  assert.equal(limits[0].rows, count); assert.equal(seeks[0].rows, count);
  assert.ok(seeks[0].dbHits <= count + 1);
  assert.match(seeks[0].arguments.Order, /source_id ASC,.*link_id ASC/);
  assert.ok(operators.every(p => p.rows <= 256));
}
function peer(path) {
  const socket = connect(path), waiting = new Map(); let buffer = Buffer.alloc(0), sequence = 0;
  const fail = error => { for (const waiter of waiting.values()) waiter.reject(error); waiting.clear(); };
  socket.on('error', fail); socket.on('close', () => fail(new Error('UDS closed')));
  socket.on('data', bytes => {
    buffer = Buffer.concat([buffer, bytes]);
    while (buffer.length >= 4 && buffer.length >= buffer.readUInt32BE() + 4) {
      const size = buffer.readUInt32BE(), reply = JSON.parse(buffer.subarray(4, size + 4)); buffer = buffer.subarray(size + 4);
      const waiter = waiting.get(reply.id); if (!waiter) return fail(new Error('unexpected RPC ID'));
      waiting.delete(reply.id); waiter.resolve(reply);
    }
  });
  return { socket, request(method, params = {}) { return new Promise((resolve, reject) => {
    const requestId = ++sequence, timer = setTimeout(() => { waiting.delete(requestId); reject(new Error('RPC timeout')); }, 10000);
    waiting.set(requestId, { resolve: value => { clearTimeout(timer); resolve(value); }, reject: error => { clearTimeout(timer); reject(error); } });
    const body = Buffer.from(JSON.stringify({ jsonrpc:'2.0', id:requestId, method, params })), header = Buffer.alloc(4); header.writeUInt32BE(body.length); socket.write(Buffer.concat([header, body]));
  }); } };
}
async function uds(root, query) {
  const child = spawn('node', [join(output, 'daemon.mjs')], { env: { ...process.env, ANAMNESIS_RUNTIME_ROOT:root, ANAMNESIS_RUNTIME_TOKEN:'ordered-probe-owned-token', ANAMNESIS_NEO4J_URI:process.env.ANAMNESIS_TEST_NEO4J_URI }, stdio:['ignore','pipe','pipe'] });
  const exited = once(child, 'exit'), lines = createInterface({ input:child.stdout }); let log = '', p;
  child.stderr.on('data', bytes => { log += bytes; });
  const ready = new Promise((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error('daemon readiness timeout')), 30000);
    lines.on('line', line => { log += line + '\n'; try { if (JSON.parse(line).event === 'listening') { clearTimeout(timer); resolve(); } } catch (error) { clearTimeout(timer); reject(error); } });
    child.once('error', error => { clearTimeout(timer); reject(error); }); child.once('exit', code => { clearTimeout(timer); reject(new Error(`early exit ${code}`)); });
  });
  try {
    await ready; p = peer(join(root, 'anamnesis.sock'));
    const denied = await p.request('graph.envelope', { seed_ids:[id(1)], T });
    const hello = await p.request('hello', { version:1, client:'g004-ordered-probe', token:'ordered-probe-owned-token', commit_mode:'receipt' });
    const result = await p.request('graph.envelope', { seed_ids:[id(1)], T });
    await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=false');
    const unavailable = await p.request('graph.envelope', { seed_ids:[id(1)], T });
    await save('uds.json', { runtime:process.version, denied, hello, result, unavailable });
    assert.equal(denied.error.data.code,'unauthenticated'); assert.ok(hello.result);
    assert.deepEqual(result.result.nodes,[id(1),id(2)]); assert.deepEqual(result.result.arcs.map(row => row.link_id),[id(101)]);
    assert.equal(unavailable.error.data.code,'degree_probe_unavailable');
    const shutdown = await p.request('shutdown'); assert.equal(shutdown.result.state, 'stopping');
    const timer = setTimeout(() => child.kill('SIGKILL'), 15000); const [code, signal] = await exited; clearTimeout(timer); assert.equal(code,0); assert.equal(signal,null);
  } finally { p?.socket.destroy(); lines.close(); if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await exited; } await writeFile(join(output, 'daemon-output.txt'), log); }
}

test('production composite seek bounds, deterministic identities and authorized physical envelope', { timeout:100000 }, async () => {
  assert.ok(process.env.ANAMNESIS_TEST_NEO4J_URI && process.env.ANAMNESIS_TEST_NEO4J_PASSWORD, 'owned runner credentials required');
  const root = await mkdtemp('/tmp/g004-ordered-probe-'), runtimeRoot = await mkdtemp('/tmp/g004-ordered-uds-');
  const driver = neo4j.driver(process.env.ANAMNESIS_TEST_NEO4J_URI, neo4j.auth.basic('neo4j',process.env.ANAMNESIS_TEST_NEO4J_PASSWORD), { disableLosslessIntegers:true });
  const engine = new Engine({ uri:process.env.ANAMNESIS_TEST_NEO4J_URI, password:process.env.ANAMNESIS_TEST_NEO4J_PASSWORD, objectsRoot:root });
  const query = async (text, params={}) => (await driver.executeQuery(text, params)).records.map(row => row.toObject());
  const observations = {};
  try {
    await engine.init(); await engine.claimWriterEpoch();
    await save('database.json', { components:await query('CALL dbms.components()'), indexes:await query('SHOW INDEXES'), constraints:await query('SHOW CONSTRAINTS') });
    await query('UNWIND $ids AS id CREATE (:Element:Episode {id:id,schema:"anamnesis.original-message/1",origin_source:"ordered-probe",time_utc:$T})', { ids:[1,2,3,4,5,6,7,8,999].map(id), T:new Date(T-1000).toISOString() });
    const highRows = Array.from({ length:1280 }, (_,i) => ({ source_id:id(999),link_id:id(10000+i),peer_id:id(2),role:'NEXT_EPISODE' })).reverse();
    await query('UNWIND $rows AS row CREATE (arc:ConductingArc) SET arc=row', { rows:highRows });
    // Neighboring sources have overlapping IDs; equality must remain an index predicate.
    await query('UNWIND $rows AS row CREATE (arc:ConductingArc) SET arc=row', { rows:[{...highRows[0],source_id:id(998),link_id:id(1)},{...highRows[0],source_id:id(1000),link_id:id(1)}] });
    const expected = Array.from({ length:256 }, (_,i) => id(10000+i));
    let productionQuery;
    const session = driver.session();
    try {
      if (process.env.G004_ORDERED_BASELINE === '1') {
        await assert.rejects(session.executeRead(tx => engine.store.graphRawProbeTx({ run:async (text, params) => {
          if (text.startsWith('EXPLAIN ')) productionQuery = text.slice(8);
          return tx.run(text, params);
        } },id(999))), error => error.code === 'ordered_probe_unavailable');
        const result = await session.run(`PROFILE ${productionQuery}`, {source:id(999)});
        await save('production-query.json', {query:productionQuery,params:{source:id(999)}});
        await save('production-profile.json', result.summary.profile);
        const operators = plans(result.summary.profile), seek = operators.find(p => /IndexSeek/.test(p.operatorType));
        assert.ok(operators.some(p => /Top/.test(p.operatorType))); assert.equal(seek.rows,1280);
        assert.deepEqual(result.records.map(r => r.get('link_id')),expected);
        const alternative = productionQuery.replace('ORDER BY arc.link_id ASC', 'ORDER BY arc.source_id ASC, arc.link_id ASC');
        assert.notEqual(alternative,productionQuery);
        const attempt = await session.run(`PROFILE ${alternative}`, {source:id(999)});
        await save('attempt-1-full-key.json', {query:alternative,profile:attempt.summary.profile,rows:attempt.records.map(r=>r.toObject())});
        bounded(attempt.summary.profile,256); assert.deepEqual(attempt.records.map(r=>r.get('link_id')),expected);
        observations.baseline = 'production refused: unordered seek consumes 1280; full-key attempt bounded';
        return;
      }
      // Invoke the production private probe, preserving its real EXPLAIN checker.
      // Only add PROFILE to execution; no copied or simplified query can pass here.
      for (const [source,count] of [[id(999),256],[id(997),0],[id(998),1]]) {
        const rows = await session.executeRead(tx => engine.store.graphRawProbeTx({ run:async (text, params) => {
          if (text.startsWith('EXPLAIN ')) { productionQuery = text.slice(8); return tx.run(text, params); }
          assert.equal(text,productionQuery);
          const result = await tx.run(`PROFILE ${text}`,params);
          await save(`profile-${source}.json`, {query:text,params,profile:result.summary.profile,rows:result.records.map(r=>r.toObject())});
          bounded(result.summary.profile,count);
          return result;
        } }, source));
        assert.equal(rows.length,count); assert.ok(rows.every(row=>row.source_id===source));
        if (source === id(999)) assert.deepEqual(rows.map(row=>row.link_id),expected);
      }
      observations.production_profiles = '1280 reverse-inserted rows: exact first256; empty and singleton sources bounded';
    } finally { await session.close(); }
    for (const [link,peer] of [[101,2],[102,3],[104,5],[105,6],[106,7]]) await query('MATCH (a:Element {id:$source}), (b:Element {id:$peer}) CREATE (a)-[:NEXT_EPISODE {id:$link,idem_key:$link}]->(b)', {source:id(1),peer:id(peer),link:id(link)});
    await query('MATCH ()-[l:NEXT_EPISODE {id:$id}]->() SET l.generation=42', {id:id(105)});
    const rows = [[101,2],[102,3],[103,4],[104,5],[105,6],[106,8]].map(([link,peer])=>({source_id:id(1),peer_id:id(peer),link_id:id(link),role:'NEXT_EPISODE',...(link===104?{generation:42}:{})}));
    await query('UNWIND $rows AS row CREATE (arc:ConductingArc) SET arc=row', {rows});
    await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=true, m.conducting_arc_revision=1');
    const generation = {id:id(500),stream:'extraction',incarnation:'a'.repeat(64),state:'active',covered_ingest_seq:1,created_at:1,updated_at:1};
    await query('CREATE (:ExtractionGeneration {id:$id,state:"active",body:$body,covered_ingest_seq:1}) MERGE (s:Meta {key:"extraction_selector"}) SET s.generation_id=$id', {id:generation.id,body:JSON.stringify(generation)});
    for (const partition of ['episodes','active_extraction']) await query('CREATE (:ExtractionCoverage {key:$id+":"+$partition,generation_id:$id,partition:$partition,covered_ingest_seq:1,body:$body})', {id:generation.id,partition,body:JSON.stringify({generation_id:generation.id,partition,required_ingest_seq:1,covered_ingest_seq:1,omission_digest:'b'.repeat(64),updated_at:1})});
    await engine.setPolicy({policy_id:id(900),scope:'content',selector:{episode_id:id(3)}},context);
    await assert.rejects(engine.store.graphEnvelope([id(1)],{T}), /unauthenticated/);
    const probe = await engine.store.probeConductingArcs(id(999),{},context);
    assert.equal(probe.count,256); assert.equal(probe.saturated,true); assert.equal('rows' in probe,false);
    const saturated = await engine.store.graphEnvelope([id(999)],{T},context);
    assert.deepEqual(saturated.nodes,[id(999)]); assert.deepEqual(saturated.arcs,[]); assert.equal(saturated.truncated,true);
    observations.envelope = 'nonempty; denied, missing link, cache/physical generation and wrong endpoint excluded; saturated source does not refill';
    await query('DROP CONSTRAINT conducting_arc_source_link');
    let refusal;
    await assert.rejects(engine.store.graphEnvelope([id(1)],{T},context), error => { refusal={name:error.name,code:error.code,message:error.message}; return error.code==='ordered_probe_unavailable'; });
    await save('missing-index.json',refusal);
    await query('CREATE CONSTRAINT conducting_arc_source_link FOR (a:ConductingArc) REQUIRE (a.source_id,a.link_id) IS UNIQUE');
    await query('CALL db.awaitIndexes(30)');
    // Corruption detection now persists UNAVAILABLE; run the corrupt-envelope
    // check last, then replace only the adversarial fixture setup for startup.
    const envelope = await engine.store.graphEnvelope([id(1)],{T},context);
    await save('envelope.json',envelope);
    assert.deepEqual(envelope.nodes,[id(1),id(2)]); assert.deepEqual(envelope.arcs.map(row=>row.link_id),[id(101)]);
    assert.equal(envelope.probes[0].count,6); assert.equal(envelope.truncated,false);
    await assert.rejects(engine.store.graphEnvelope([id(1)],{T},context),error=>error.code==='degree_probe_unavailable');
    await query('MATCH ()-[l:NEXT_EPISODE]->() WHERE NOT l.id IN $ids DELETE l',{ids:[id(101),id(102)]});
    await engine.rebuildConductingArcs();
    await engine.close();
    await uds(runtimeRoot, query);
    observations.uds = 'authenticated nonempty envelope and typed coverage refusal; clean daemon shutdown';
  } catch (error) { await save('failure.json',{error:String(error),stack:error.stack}); throw error; }
  finally { await save('assertions.json',observations); await engine.close(); await driver.close(); await rm(root,{recursive:true,force:true}); await rm(runtimeRoot,{recursive:true,force:true}); await save('fixture-cleanup.json',{objectsRoot:root,runtimeRoot,removed:true,driversClosed:true}); }
});
