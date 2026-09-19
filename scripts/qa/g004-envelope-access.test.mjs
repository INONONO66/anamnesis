import test from 'node:test';
import assert from 'node:assert/strict';
import neo4j from 'neo4j-driver';
import { Engine } from '../../packages/core/src/engine.ts';
import { randomBytes } from 'node:crypto';
const id=()=>{const b=randomBytes(16); b[6]=(b[6]&15)|112; b[8]=(b[8]&63)|128; const h=b.toString('hex'); return `${h.slice(0,8)}-${h.slice(8,12)}-${h.slice(12,16)}-${h.slice(16,20)}-${h.slice(20)}`;};
test('G004 real bounded ConductingArc probe and fail-closed envelope',async()=>{
 const uri=process.env.ANAMNESIS_TEST_NEO4J_URI, password=process.env.ANAMNESIS_TEST_NEO4J_PASSWORD; assert.ok(uri&&password);
 const context={ principal:'installation', commit_mode:'receipt' };
 const d=neo4j.driver(uri,neo4j.auth.basic('neo4j',password),{disableLosslessIntegers:true}); const e=new Engine({uri,password,objectsRoot:'/tmp/g004-envelope-objects'});
 try { await d.executeQuery('MATCH (n) DETACH DELETE n'); await e.init(); await e.claimWriterEpoch();
  const source=id(), generation=id(), peers=Array.from({length:257},id), links=peers.map((peer,i)=>({source_id:source,link_id:id(),peer_id:peer,role:i%2?'MENTIONS':'NEXT_EPISODE',generation:i%2?generation:null}));
  const generationRecord={id:generation,stream:'extraction',incarnation:'a'.repeat(64),state:'catching_up',covered_ingest_seq:0,created_at:1,updated_at:1};
  await e.store.createExtractionGeneration(generationRecord,context);
  for (const partition of ['episodes','active_extraction']) await e.store.recordExtractionCoverage({generation_id:generation,partition,expected_covered_ingest_seq:0,covered_ingest_seq:0},context);
  const activeGeneration={...generationRecord,state:'active'};
  await d.executeQuery('MATCH (g:ExtractionGeneration {id:$generation}) SET g.state="active",g.body=$body MERGE (s:Meta {key:"extraction_selector"}) SET s.generation_id=$generation',{generation,body:JSON.stringify(activeGeneration)});
  await d.executeQuery('MERGE (m:Meta {key:"meta"}) SET m.conducting_arc_ready=true, m.conducting_arc_revision=1');
  await d.executeQuery('CREATE (:Element:Episode {id:$id,schema:"anamnesis.original-message/1",origin_source:"g004-envelope",time_utc:"2026-01-01T00:00:00Z"})',{id:source});
  await d.executeQuery('UNWIND $rows AS r CREATE (:ConductingArc {source_id:r.source_id,link_id:r.link_id,peer_id:r.peer_id,role:r.role,generation:r.generation})',{rows:links});
  const probe=await e.store.probeConductingArcs(source,{},context); assert.equal(probe.count,256); assert.equal(probe.saturated,true); assert.equal('rows' in probe,false);
  const envelope=await e.store.graphEnvelope([source],{},context); assert.ok(envelope.nodes.length<=2000); assert.ok(envelope.arcs.length<=20000); assert.equal(envelope.probes[0].count,256);
  await d.executeQuery('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=false'); await assert.rejects(e.store.graphEnvelope([source],{},context),/degree_probe_unavailable/);
 } finally { await e.close(); await d.executeQuery('MATCH (n) DETACH DELETE n').catch(()=>{}); await d.close(); }
});
