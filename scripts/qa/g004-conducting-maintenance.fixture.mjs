import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { join } from 'node:path';
import { once } from 'node:events';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { connect } from 'node:net';
import neo4j from 'neo4j-driver';
import { Engine } from '../../packages/core/src/engine.ts';
const context = { principal:'installation', commit_mode:'receipt' };
const id = n => `01900000-0000-7000-8000-${n.toString(16).padStart(12,'0')}`;
const output = process.env.G004_MAINTENANCE_OUTPUT;
const save = (name, data) => writeFile(join(output,name), JSON.stringify(data,null,2));
const roles = ['NEXT_EPISODE','MENTIONS','RELATES_TO','HAS_MEMBER','DERIVED_FROM'];
const episode = (record, day) => ({schema:'anamnesis.original-message/1',content:`maintenance ${record}`,origin:{source:'maintenance',session:'ordered',actor:'user',record},time:{value:`2026-01-${String(day).padStart(2,'0')}T00:00:00Z`,precision:'second'},mass:0.5,properties:{}});

async function runtimeIngestion(options,query) {
  const root=await mkdtemp('/tmp/g004-maintenance-uds-');
  const child=spawn('node',[join(output,'daemon.mjs')],{env:{...process.env,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:'maintenance-owned-token',ANAMNESIS_NEO4J_URI:options.uri,ANAMNESIS_NEO4J_PASSWORD:options.password},stdio:['ignore','pipe','pipe']});
  const exited=once(child,'exit'), lines=createInterface({input:child.stdout}); let log='',socket;
  child.stderr.on('data',bytes=>{log+=bytes;});
  const ready=new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>reject(new Error('daemon readiness timeout')),30000);
    lines.on('line',line=>{log+=line+'\n';try {if(JSON.parse(line).event==='listening'){clearTimeout(timer);resolve();}}catch(error){clearTimeout(timer);reject(error);}});
    child.once('error',error=>{clearTimeout(timer);reject(error);});child.once('exit',code=>{clearTimeout(timer);reject(new Error(`early exit ${code}`));});
  });
  try {
    await ready; socket=connect(join(root,'anamnesis.sock'));
    const waiting=new Map();let buffer=Buffer.alloc(0),sequence=0;
    const fail=error=>{for(const waiter of waiting.values())waiter.reject(error);waiting.clear();};
    socket.on('error',fail);socket.on('close',()=>fail(new Error('UDS closed')));
    socket.on('data',bytes=>{buffer=Buffer.concat([buffer,bytes]);while(buffer.length>=4&&buffer.length>=buffer.readUInt32BE()+4){const size=buffer.readUInt32BE(),reply=JSON.parse(buffer.subarray(4,size+4));buffer=buffer.subarray(size+4);const waiter=waiting.get(reply.id);if(!waiter)return fail(new Error('unexpected RPC id'));waiting.delete(reply.id);waiter.resolve(reply);}});
    const request=(method,params={})=>new Promise((resolve,reject)=>{const n=++sequence,timer=setTimeout(()=>{waiting.delete(n);reject(new Error('RPC timeout'));},15000);waiting.set(n,{resolve:value=>{clearTimeout(timer);resolve(value);},reject:error=>{clearTimeout(timer);reject(error);}});const body=Buffer.from(JSON.stringify({jsonrpc:'2.0',id:n,method,params})),header=Buffer.alloc(4);header.writeUInt32BE(body.length);socket.write(Buffer.concat([header,body]));});
    assert.ok((await request('hello',{version:1,client:'g004-maintenance',token:'maintenance-owned-token',commit_mode:'receipt'})).result);
    const responses=[];
    for(const [record,day] of [['uds-a',6],['uds-c',8],['uds-b',7]]) {const reply=await request('remember',{episode:episode(record,day),source_revision:record,expected_previous_revision_key:null});assert.ok(reply.result,JSON.stringify(reply));responses.push(reply);}
    const links=await query('MATCH (a:Episode)-[l:NEXT_EPISODE]->(b:Episode) WHERE a.origin_record STARTS WITH "uds-" AND b.origin_record STARTS WITH "uds-" RETURN a.origin_record AS a,b.origin_record AS b,l.id AS id ORDER BY a');
    assert.deepEqual(links.map(l=>[l.a,l.b]),[['uds-a','uds-b'],['uds-b','uds-c']]);
    const arcs=await query('MATCH (a:ConductingArc) WHERE a.link_id IN $ids RETURN properties(a) AS arc',{ids:links.map(l=>l.id)});assert.equal(arcs.length,4);
    await save('node-runtime.json',{runtime:process.version,responses,links,arcs});
    assert.equal((await request('shutdown')).result.state,'stopping');const timer=setTimeout(()=>child.kill('SIGKILL'),15000);const [code,signal]=await exited;clearTimeout(timer);assert.equal(code,0);assert.equal(signal,null);
  } finally {socket?.destroy();lines.close();if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await exited;}await writeFile(join(output,'daemon-output.txt'),log);await rm(root,{recursive:true,force:true});await save('runtime-cleanup.json',{root,removed:true,exitCode:child.exitCode,signal:child.signalCode});}
}

test('physical writes, coverage and bounded repair', {timeout:150000}, async () => {
  assert.ok(process.env.ANAMNESIS_TEST_NEO4J_URI && process.env.ANAMNESIS_TEST_NEO4J_PASSWORD,'owned credentials required');
  const root = await mkdtemp('/tmp/g004-maintenance-');
  const options = {uri:process.env.ANAMNESIS_TEST_NEO4J_URI,password:process.env.ANAMNESIS_TEST_NEO4J_PASSWORD,objectsRoot:root};
  const driver = neo4j.driver(options.uri,neo4j.auth.basic('neo4j',options.password),{disableLosslessIntegers:true});
  const engine = new Engine(options);
  const query = async (text,params={}) => (await driver.executeQuery(text,params)).records.map(r=>r.toObject());
  const physical = () => query('MATCH (a)-[l]->(b) WHERE type(l) IN $roles RETURN a.id AS a,b.id AS b,l.id AS link_id,type(l) AS role,l.generation AS generation,CASE WHEN type(l)="HAS_MEMBER" THEN a.source_extraction_generation ELSE null END AS source_extraction_generation ORDER BY link_id',{roles});
  const rows = () => query('MATCH (a:ConductingArc) RETURN a.source_id AS source_id,a.link_id AS link_id,a.peer_id AS peer_id,a.role AS role,a.generation AS generation,a.source_extraction_generation AS source_extraction_generation ORDER BY source_id,link_id');
  const state = () => query('MATCH (m:Meta {key:"meta"}) RETURN m.conducting_arc_ready AS ready,m.conducting_arc_revision AS revision');
  const identity = () => query('MATCH (a:ConductingArc) RETURN elementId(a) AS identity,properties(a) AS properties ORDER BY a.source_id,a.link_id');
  const coverage = () => query('MATCH (c:ConductingArcCoverage) RETURN elementId(c) AS identity,properties(c) AS properties ORDER BY c.stream,toString(c.generation)');
  const compare = async name => {
    const links = await physical(), actual = await rows();
    const expected = links.flatMap(l=>[...new Set([l.a,l.b])].map(source_id=>({source_id,link_id:l.link_id,peer_id:source_id===l.a?l.b:l.a,role:l.role,generation:l.generation,source_extraction_generation:l.source_extraction_generation}))).sort((a,b)=>a.source_id.localeCompare(b.source_id)||a.link_id.localeCompare(b.link_id));
    await save(`${name}.json`,{links,actual,expected,state:await state(),coverage:await coverage()});
    assert.deepEqual(actual,expected,`${name}: physical endpoint coverage`);
  };
  const failures = [];
  const seam = async (name,run) => { try { await run(); } catch(error) { failures.push({name,error:String(error),stack:error.stack}); if (!process.env.G004_MAINTENANCE_RED) throw error; } };
  try {
    await engine.init(); await engine.claimWriterEpoch();
    await save('database.json',{components:await query('CALL dbms.components()'),constraints:await query('SHOW CONSTRAINTS')});
    await seam('fresh readiness',async()=>assert.equal((await state())[0].ready,true));
    const generation={id:id(500),stream:'extraction',incarnation:'a'.repeat(64),state:'catching_up',covered_ingest_seq:0,created_at:1,updated_at:1};
    await engine.store.createExtractionGeneration(generation,context);
    await seam('generation opening coverage',async()=>assert.ok((await coverage()).some(row=>row.properties.stream==='extraction'&&row.properties.generation===generation.id&&row.properties.state==='COMPLETE')));
    for (const partition of ['episodes','active_extraction']) await engine.store.recordExtractionCoverage({generation_id:generation.id,partition,expected_covered_ingest_seq:0,covered_ingest_seq:0},context);
    await query('MATCH (m:Meta {key:"meta"}) SET m.conducting_arc_ready=false');
    await seam('cutover refuses incomplete conducting coverage',()=>assert.rejects(engine.store.cutoverExtractionGeneration({generation_id:generation.id,expected_generation_id:null,expected_selector_version:0},context),/degree_probe_unavailable/));
    // Only initialization may republish a verified empty retained graph.
    await engine.init();
    const a = await engine.remember(episode('a',1)), c = await engine.remember(episode('c',3));
    await seam('remember creates cache',()=>compare('insert'));
    const old = (await physical())[0].link_id;
    await query('CREATE (:HubArc {hub_id:$source,link_id:$link,rank:0})',{source:a.id,link:old});
    const b = await engine.remember(episode('b',2));
    await seam('out of order rewire',async()=>{await compare('rewire');assert.deepEqual(await query('MATCH (h:HubArc {link_id:$id}) RETURN h',{id:old}),[]);});
    await query('CREATE (:Element:Fact {id:$f,generation:42}),(:Element:Entity {id:$e,generation:42}),(:Element:Entity {id:$e2,generation:42}),(:Element:Community {id:$c,generation:7,source_extraction_generation:42}),(:Generation {stream:"community",generation:7,source_extraction_generation:42,state:"RETIRED"}),(:Generation {stream:"extraction",generation:42,state:"BUILDING"})',{f:id(10),e:id(11),e2:id(12),c:id(13)});
    for (const [n,role,from,to] of [[101,'MENTIONS',a.id,id(11)],[102,'RELATES_TO',id(11),id(12)],[103,'HAS_MEMBER',id(13),id(10)],[104,'DERIVED_FROM',id(10),a.id]]) {
      await engine.store.putLink({id:id(n),from,to,role,content:`physical ${role}`});
      await seam(`putLink ${role}`,()=>compare(role));
    }
    await seam('physical generation metadata',async()=>{ const links=await physical();assert.equal(links.find(l=>l.role==='HAS_MEMBER').generation,7);assert.equal(links.find(l=>l.role==='MENTIONS').generation,42); });
    await engine.store.rebuildTopology();
    await seam('topology rebuild',()=>compare('topology-rebuild'));
    if (process.env.G004_MAINTENANCE_RED) { await save('red-seams.json',failures); assert.deepEqual(failures,[]); return; }
    const receipt = await engine.issueReceipt({recall_id:id(900),primary_ids:[a.id,b.id,c.id]},context);
    await engine.commitReceipt({operation_id:id(901),recall_id:receipt.recall_id,adopted:[a.id],reward:1},context);
    const originals = () => query('MATCH (n) WHERE n:Episode OR n:Hit OR n:RecallReceipt RETURN labels(n) AS labels,properties(n) AS properties ORDER BY coalesce(n.id,n.recall_id)');
    const immutable = await originals();
    const snapshot = {identity:await identity(),coverage:await coverage(),state:await state()};
    assert.equal((await engine.remember(episode('b',2))).created,false);
    const duplicate = await engine.store.putLink({id:id(201),from:a.id,to:id(11),role:'MENTIONS',content:'physical MENTIONS'});
    assert.equal(duplicate.id,id(101));
    assert.deepEqual({identity:await identity(),coverage:await coverage(),state:await state()},snapshot);
    await save('duplicate.json',snapshot);
    await engine.store.putLink({id:id(202),from:id(11),to:id(12),role:'RELATES_TO',content:'parallel retained physical relation'});
    await engine.store.putLink({id:id(203),from:c.id,to:a.id,role:'INVALIDATES',content:'nonconducting'});
    await query('CREATE (:Element:Fact {id:$id,generation:42})',{id:id(14)});
    await engine.store.putLink({id:id(204),from:id(10),to:id(14),role:'CONTRASTS',content:'nonconducting'});
    await compare('parallel-nonconducting');
    await assert.rejects(engine.store.putLink({id:id(205),from:id(10),to:id(10),role:'DERIVED_FROM',content:'self'}));
    const verify = await engine.verifyConductingArcs();
    assert.deepEqual(verify.issues,[]); assert.equal(verify.ready,true);
    await assert.rejects(engine.store.cutoverExtractionGeneration({generation_id:generation.id,expected_generation_id:null,expected_selector_version:0},context),{code:'coverage_incomplete'});
    // Retained active-state fixture for maintenance/graph tests only. This is
    // not activation evidence; production refuses missing derived proofs.
    await query('MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:"extraction_selector"}) SET g.state="active",g.body=$body,s.generation_id=$id,s.selector_version=1',{id:generation.id,body:JSON.stringify({...generation,state:'active'})});
    const raw=(await rows()).find(row=>row.source_id===b.id&&row.role==='NEXT_EPISODE');
    await query('MATCH (a:ConductingArc {source_id:$source,link_id:$link}) SET a.peer_id=$bad',{source:b.id,link:raw.link_id,bad:id(999)});
    // Instrument the exact probe transaction supplied by graphEnvelope, without
    // prewarming in a separate read transaction or changing its strict validator.
    const originalProbe=engine.store.graphRawProbeTx;
    engine.store.graphRawProbeTx=async function(tx,source){return originalProbe.call(this,{run:async(text,params)=>{
      const explain=text.startsWith('EXPLAIN '),result=await tx.run(explain?text:`PROFILE ${text}`,params);
      await save(explain?'production-probe-plan.json':'production-probe-profile.json',{query:text,params,plan:result.summary.plan,profile:result.summary.profile});
      if(!explain){
        const flatten=plan=>[plan,...(plan.children??[]).flatMap(flatten)];
        const operators=flatten(result.summary.profile),limits=operators.filter(p=>/^Limit(?:@|$)/.test(p.operatorType));
        assert.ok(!operators.some(p=>/Top|Sort|Scan|Expand/.test(p.operatorType)));
        assert.equal(limits.length,1);assert.equal(limits[0].arguments.Details,'256');
        assert.equal(limits[0].children.length,1);
        const seek=limits[0].children[0];assert.match(seek.operatorType,/^NodeUniqueIndexSeek(?:@|$)/);
        assert.equal(seek.arguments.Order,'arc.source_id ASC, arc.link_id ASC');
        assert.equal(result.records.length,2);assert.equal(limits[0].rows,2);assert.equal(seek.rows,2);
        assert.ok(seek.dbHits<=3);assert.ok(operators.every(p=>p.rows<=256));
      }
      return result;
    }},source);};
    let excluded;
    try {excluded=await engine.graphEnvelope([b.id],{},context);}finally{engine.store.graphRawProbeTx=originalProbe;}
    const afterDetection=await state();await save('online-detection.json',{excluded,afterDetection});
    assert.equal(afterDetection[0].ready,false,'detected stale physical row invalidates the next probe');
    await assert.rejects(engine.graphEnvelope([b.id],{},context),error=>error.code==='degree_probe_unavailable');
    await engine.rebuildConductingArcs();
    await engine.setPolicy({policy_id:id(902),scope:'content',selector:{episode_id:a.id}},context);
    await compare('policy-independent');
    const physicalBefore = await physical(), structureBefore = await query('MATCH (m:Meta {key:"meta"}) RETURN m.structure_revision AS revision');
    const repaired = await engine.rebuildConductingArcs();
    assert.deepEqual(repaired.issues,[]); assert.equal(repaired.ready,true);
    await compare('explicit-rebuild'); assert.deepEqual(await physical(),physicalBefore);
    assert.deepEqual(await query('MATCH (m:Meta {key:"meta"}) RETURN m.structure_revision AS revision'),structureBefore);
    await query('MATCH (a:ConductingArc {link_id:$id}) SET a.peer_id=$bad',{id:id(101),bad:id(999)});
    const corruptSnapshot={identity:await identity(),coverage:await coverage(),state:await state()};
    const corrupt=await engine.verifyConductingArcs();
    assert.ok(corrupt.issues.some(issue=>issue.startsWith('mismatched-row:')));
    assert.deepEqual({identity:await identity(),coverage:await coverage(),state:await state()},corruptSnapshot,'verify is read-only');
    const blocked=await engine.checkConductingArcs();assert.equal(blocked.ready,false);
    await assert.rejects(engine.graphEnvelope([b.id],{},context),error=>error.code==='degree_probe_unavailable');
    await assert.rejects(engine.rebuildConductingArcs({maxItems:1}),/maintenance_limit_exceeded/);
    assert.equal((await state())[0].ready,false); assert.deepEqual(await identity(),corruptSnapshot.identity,'overflow never clears partial rows');
    await save('corruption-and-limit.json',{corrupt,blocked,state:await state()});
    await engine.rebuildConductingArcs(); await compare('corruption-repaired');
    // Non-serving BUILDING/RETIRED partitions remain physical authority. Invalid
    // coverage must be reported and fenced even when all endpoint rows match.
    await query('MATCH (c:ConductingArcCoverage {stream:"community",generation:7}) SET c.state="INVALID"');
    const hidden=await engine.verifyConductingArcs();
    assert.ok(hidden.partitions.some(p=>p.stream==='extraction'&&p.generation===42));
    assert.ok(hidden.issues.includes('coverage-incomplete:{"stream":"community","generation":7}'));
    assert.equal((await engine.checkConductingArcs()).ready,false);
    await assert.rejects(engine.graphEnvelope([b.id],{},context),error=>error.code==='degree_probe_unavailable');
    await save('hidden-invalid-coverage.json',{hidden,state:await state(),coverage:await coverage()});
    assert.deepEqual((await engine.rebuildConductingArcs()).issues,[]);
    await compare('hidden-coverage-repaired');
    // An invalid retained physical ID cannot be certified by reconstruction.
    await query('MATCH (a:Element {id:$a}),(b:Element {id:$b}) CREATE (a)-[:RELATES_TO {id:"invalid",generation:42}]->(b)',{a:id(11),b:id(12)});
    const invalidRows=await identity();
    await assert.rejects(engine.rebuildConductingArcs(),/conducting_physical_invalid/);
    assert.equal((await state())[0].ready,false);assert.deepEqual(await identity(),invalidRows);
    await save('invalid-physical.json',await engine.verifyConductingArcs());
    await query('MATCH ()-[l:RELATES_TO {id:"invalid"}]->() DELETE l');
    await engine.rebuildConductingArcs();
    // Simulate an older retained store: only physical links survive cache loss.
    await query('MATCH (a:ConductingArc) DELETE a'); await query('MATCH (c:ConductingArcCoverage) DELETE c');
    await engine.init(); assert.equal((await state())[0].ready,false);assert.deepEqual(await rows(),[]);
    await engine.remember(episode('d',4));
    assert.equal((await state())[0].ready,false,'append cannot certify old uncovered links');
    const incomplete=await engine.verifyConductingArcs();assert.ok(incomplete.issues.some(issue=>issue.startsWith('missing-row:')));
    await engine.rebuildConductingArcs();await compare('existing-data-rebuild');
    // Legacy physical self-loop: exactly one reconstructed row, explicit violation.
    await query('MATCH (a:Element {id:$source}) CREATE (a)-[:RELATES_TO {id:$id,generation:42}]->(a)',{source:id(11),id:id(206)});
    const self=await engine.rebuildConductingArcs();assert.ok(self.issues.includes(`self-link:${id(206)}`));
    assert.equal((await rows()).filter(row=>row.link_id===id(206)).length,1);
    await query('MATCH ()-[l:RELATES_TO {id:$id}]->() DELETE l',{id:id(206)});
    await engine.rebuildConductingArcs();
    await assert.rejects(engine.store.putLink({id:id(101),from:id(11),to:id(12),role:'RELATES_TO',content:'cross-role collision'}),/conducting_link_id_collision/);
    await compare('collision-rollback');
    const retainedOriginals=await originals();
    assert.deepEqual(retainedOriginals.filter(row=>immutable.some(old=>JSON.stringify(old)===JSON.stringify(row))),immutable);
    const contender=new Engine(options); await contender.claimWriterEpoch();
    const fenced={identity:await identity(),coverage:await coverage(),state:await state(),links:await physical()};
    try {
      await assert.rejects(engine.remember(episode('stale',5)),/stale_writer_epoch/);
      await assert.rejects(engine.store.putLink({id:id(207),from:id(11),to:id(12),role:'RELATES_TO',content:'stale'}),/stale_writer_epoch/);
      await assert.rejects(engine.store.rebuildTopology(),/stale_writer_epoch/);
      await assert.rejects(engine.rebuildConductingArcs(),/stale_writer_epoch/);
      assert.deepEqual({identity:await identity(),coverage:await coverage(),state:await state(),links:await physical()},fenced);
      await save('stale-writer.json',fenced);
    } finally {await contender.close();}
    await save('originals.json',{before:immutable,after:retainedOriginals});
    assert.deepEqual(await originals(),retainedOriginals);
    await runtimeIngestion(options,query);
    await compare('built-node-ingestion');
    assert.deepEqual((await originals()).filter(row=>retainedOriginals.some(old=>JSON.stringify(old)===JSON.stringify(row))),retainedOriginals);
  } finally {
    await save('failures.json',failures); await engine.close(); await driver.close(); await rm(root,{recursive:true,force:true});
    await save('cleanup.json',{root,removed:true,driversClosed:true});
  }
});
