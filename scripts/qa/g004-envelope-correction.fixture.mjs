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
const output = process.env.G004_CORRECTION_OUTPUT;
const save = (name, value) => writeFile(join(output, name), JSON.stringify(value, null, 2));
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
async function uds(root, check) {
 const child = spawn('node', [join(output, 'daemon.mjs')], { env: { ...process.env, ANAMNESIS_RUNTIME_ROOT:root, ANAMNESIS_RUNTIME_TOKEN:'envelope-correction-owned-token', ANAMNESIS_NEO4J_URI:process.env.ANAMNESIS_TEST_NEO4J_URI }, stdio:['ignore','pipe','pipe'] });
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
  const hello = await p.request('hello', { version:1, client:'g004-correction', token:'envelope-correction-owned-token', commit_mode:'receipt' });
  const result = await p.request('graph.envelope', { seed_ids:[id(1)], T });
  await save('uds.json', { runtime:process.version, denied, hello, result });
  await check({ denied, hello, result, p });
  const shutdown = await p.request('shutdown'); assert.equal(shutdown.result.state, 'stopping');
  const timer = setTimeout(() => child.kill('SIGKILL'), 15000); const [code, signal] = await exited; clearTimeout(timer); assert.equal(code,0); assert.equal(signal,null);
 } finally { p?.socket.destroy(); lines.close(); if (child.exitCode === null && child.signalCode === null) { child.kill('SIGKILL'); await exited; } await writeFile(join(output, 'daemon-output.txt'), log); }
}

test('real physical graph authorization, validation, boundaries and raw UDS refusal', { timeout:100000 }, async () => {
 assert.ok(process.env.ANAMNESIS_TEST_NEO4J_URI && process.env.ANAMNESIS_TEST_NEO4J_PASSWORD, 'owned runner credentials required');
 const root = await mkdtemp('/tmp/g004-envelope-correction-'), runtimeRoot = await mkdtemp('/tmp/g004-envelope-uds-');
 const driver = neo4j.driver(process.env.ANAMNESIS_TEST_NEO4J_URI, neo4j.auth.basic('neo4j',process.env.ANAMNESIS_TEST_NEO4J_PASSWORD), { disableLosslessIntegers:true });
 const engine = new Engine({ uri:process.env.ANAMNESIS_TEST_NEO4J_URI, password:process.env.ANAMNESIS_TEST_NEO4J_PASSWORD, objectsRoot:root });
 const query = async (text, params={}) => (await driver.executeQuery(text, params)).records.map(row => row.toObject());
 const failures = [], observations = {};
 const check = async (name, run) => { try { await run(); observations[name] = 'pass'; } catch (error) { failures.push({ name, error:String(error), stack:error.stack }); observations[name] = 'FAIL'; } };
 try {
  await engine.init(); await engine.claimWriterEpoch();
  await query('CALL db.awaitIndexes(30)');
  await query('UNWIND $ids AS id CREATE (:Element:Episode {id:id,schema:"anamnesis.original-message/1",origin_source:"correction",time_utc:$T})', { ids:[1,2,3,4,5,6,7,8].map(id), T:new Date(T-1000).toISOString() });
  await query('MATCH (a:Element {id:$source}), (b:Element {id:$peer}) CREATE (a)-[:NEXT_EPISODE {id:$link,idem_key:$link}]->(b)', { source:id(1), peer:id(2), link:id(101) });
  await query('MATCH (a:Element {id:$source}), (b:Element {id:$peer}) CREATE (a)-[:NEXT_EPISODE {id:$link,idem_key:$link}]->(b)', { source:id(1), peer:id(3), link:id(102) });
  const rows = [ [101,2], [102,3], [103,4] ].map(([link,peer])=>({ source_id:id(1),peer_id:id(peer),link_id:id(link),role:'NEXT_EPISODE' }));
  await query('UNWIND $rows AS row CREATE (a:ConductingArc) SET a=row', { rows });
  await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=true, m.conducting_arc_revision=1');
  const generation = { id:id(500),stream:'extraction',incarnation:'a'.repeat(64),state:'active',covered_ingest_seq:1,created_at:1,updated_at:1 };
  await query('CREATE (:ExtractionGeneration {id:$id,state:"active",body:$body,covered_ingest_seq:1}) MERGE (s:Meta {key:"extraction_selector"}) SET s.generation_id=$id', { id:generation.id,body:JSON.stringify(generation) });
  for (const partition of ['episodes','active_extraction']) await query('CREATE (:ExtractionCoverage {key:$id+":"+$partition,generation_id:$id,partition:$partition,covered_ingest_seq:1,body:$body})', { id:generation.id,partition,body:JSON.stringify({ generation_id:generation.id,partition,required_ingest_seq:1,covered_ingest_seq:1,omission_digest:'b'.repeat(64),updated_at:1 }) });
  await engine.setPolicy({ policy_id:id(900),scope:'content',selector:{episode_id:id(3)} },context);
  await check('authentication', ()=>assert.rejects(engine.store.graphEnvelope([id(1)],{T}),/unauthenticated/));
  await check('probe IDs are private', async ()=> { const probe=await engine.store.probeConductingArcs(id(1),{},context); observations.probe=probe; assert.equal('rows' in probe,false); assert.equal(probe.count,3); });
  await check('physical nonempty and policy exclusion', async ()=> {
   const result = await engine.store.graphEnvelope([id(1)],{T},context); observations.envelope = result;
   assert.deepEqual(result.nodes,[id(1),id(2)]); assert.deepEqual(result.arcs.map(row=>row.link_id),[id(101)]);
   assert.equal(JSON.stringify(result).includes(id(3)),false); assert.equal(JSON.stringify(result).includes(id(103)),false);
  });
  await query('UNWIND range(1,1024) AS n CREATE (:ConductingArc {source_id:$source,link_id:toString(n)})', {source:id(999)});
  const profile = await driver.executeQuery(`PROFILE MATCH (arc:ConductingArc {source_id:$source}) USING INDEX SEEK arc:ConductingArc(source_id,link_id) WHERE arc.link_id > '' RETURN arc.source_id AS source_id,arc.link_id AS link_id,arc.peer_id AS peer_id,arc.role AS role,arc.generation AS generation,arc.source_extraction_generation AS source_extraction_generation ORDER BY arc.source_id ASC, arc.link_id ASC LIMIT 256`, {source:id(999)});
  await save('typed-range-profile.json',profile.summary.profile);
  await check('bounded ordered high degree plan', async ()=> { const plans=[]; const visit=p=>{plans.push(p);for(const child of p.children??[])visit(child);};visit(profile.summary.profile); assert.ok(plans.some(p=>/Limit/.test(p.operatorType))); assert.ok(!plans.some(p=>/Top|Sort/.test(p.operatorType))); const seek=plans.find(p=>/IndexSeek/.test(p.operatorType));assert.ok(seek);assert.ok(seek.rows<=256); });
  await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=false');
  await engine.close();
  await check('typed real UDS refusal', ()=>uds(runtimeRoot,async ({denied,hello,result})=> { assert.equal(denied.error.data.code,'unauthenticated'); assert.ok(hello.result); assert.equal(result.error.code,-32000); assert.equal(result.error.data.code,'degree_probe_unavailable'); }));
 } finally {
  await save('assertions.json',{ observations,failures }); await engine.close(); await driver.close(); await rm(root,{recursive:true,force:true}); await rm(runtimeRoot,{recursive:true,force:true});
 }
 assert.deepEqual(failures,[]);
});
