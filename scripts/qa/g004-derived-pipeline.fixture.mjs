import test from 'node:test';
import assert from 'node:assert/strict';
import {createHash,randomBytes} from 'node:crypto';
import {mkdtemp,rm} from 'node:fs/promises';
import {createServer} from 'node:http';
import {once,EventEmitter} from 'node:events';
import {spawn} from 'node:child_process';
import {createInterface} from 'node:readline';
import {connect} from 'node:net';
import neo4j from 'neo4j-driver';
import {Engine} from '../../packages/core/src/engine.ts';
import {HttpExtractionProvider} from '../../packages/core/src/extraction.ts';
import {RpcResponse} from '../../packages/protocol/src/rpc.ts';
const uuid=()=>`01900000-0000-7000-8000-${randomBytes(6).toString('hex')}`;
const hash=text=>createHash('sha256').update(text).digest('hex');
const context={principal:'installation',commit_mode:'receipt'};
const incarnation=hash('g004-claim-judge-audit-v1');
const options={uri:process.env.ANAMNESIS_TEST_NEO4J_URI,password:process.env.ANAMNESIS_TEST_NEO4J_PASSWORD};
const dispositions=['retain','suppress','correct','unknown'];
const claims=text=>({task:'claim',claims:dispositions.map(()=>({text:'fixture assertion',evidence:{start:0,end:Buffer.byteLength(text),text}})),language:'und',modality:'text'});
const judgment=input=>({task:'judge_claims',claim_body_digest:input.claim_context.body_digest,decisions:input.claim_context.claims.map((claim,claim_index)=>({claim_index,disposition:dispositions[claim_index],evidence:claim.evidence})),language:'und',modality:'text'});
const output=input=>input.task==='claim'?claims(input.text):judgment(input);
async function setup(handler=async input=>output(input)) {
 const root=await mkdtemp('/tmp/g004-pipeline-');let now=100;const requests=[],errors=[];
 const events=new EventEmitter();
 const server=createServer((req,res)=>{void(async()=>{const chunks=[];for await(const part of req)chunks.push(part);const input=JSON.parse(Buffer.concat(chunks));requests.push(input);const result=await handler(input,events);res.end(JSON.stringify({model:'qa-pipeline',model_incarnation:incarnation,output:result}));})().catch(error=>{errors.push(String(error));res.statusCode=500;res.end();});});
 const listening=once(server,'listening');server.listen(0,'127.0.0.1');await listening;
 const config={endpoint:`http://127.0.0.1:${server.address().port}`,model:'qa-pipeline',model_incarnation:incarnation,timeout_ms:5000};
 const engine=new Engine({...options,objectsRoot:root,clock:()=>now,extractionProvider:new HttpExtractionProvider(config)});
 const driver=neo4j.driver(options.uri,neo4j.auth.basic('neo4j',options.password),{disableLosslessIntegers:true});
 const query=async(cypher,params={})=>(await driver.executeQuery(cypher,params)).records.map(r=>r.toObject());
 await query('MATCH (n) DETACH DELETE n');await engine.init();await engine.claimWriterEpoch();
 const g={id:uuid(),stream:'extraction',incarnation,state:'catching_up',covered_ingest_seq:0,created_at:100,updated_at:100};
 await engine.store.createExtractionGeneration(g,context);
 const original={content:'Aé🙂Z',time:{value:'2026-09-01T00:00:00Z',precision:'second'},origin:{source:root,session:root,actor:'qa',record:'source'},source_revision:'v1',expected_previous_revision_key:null};
 const source=await engine.remember(original);
 const create=()=>engine.createExtractionPipeline({id:uuid(),generation_id:g.id,source_id:source.id},context);
 const run=task=>engine.runExtractionPipeline({task_id:task.id,expected_version:task.version,worker_id:'qa',lease_ms:30000},context);
 return {root,engine,store:engine.store,query,g,source,original,create,run,requests,config,events,clock:v=>{now=v},async close(){await engine.close();await driver.close();const closed=once(server,'close');server.close();server.closeAllConnections();await closed;await rm(root,{recursive:true,force:true});assert.deepEqual(errors,[]);console.log(JSON.stringify({checkpoint:'fixture-cleanup',root,removed:true}));}};
}
const originals=f=>f.query('MATCH (e:Episode) RETURN properties(e) AS p ORDER BY e.id');
const semantic=f=>f.query('MATCH (e:Fact|Entity|Community) RETURN count(e) AS count');
const audit=(checkpoint,value)=>console.log(JSON.stringify({checkpoint,...value}));
const lease=(task)=>({task_id:task.id,expected_version:task.version,worker_id:'qa',lease_ms:30000});

test('real HTTP claim then judge retains immutable parent binding and four dispositions without semantic writes',async()=>{
 const f=await setup();try{
  const before=await originals(f),task=await f.create(),result=await f.run(task);
  assert.equal(result.state,'known');assert.equal(result.semantic_writes,false);assert.equal(result.claim.state,'succeeded');assert.equal(result.judge.state,'succeeded');
  assert.deepEqual(f.requests.map(r=>r.task),['claim','judge_claims']);
  assert.equal(f.requests[1].claim_context.attempt_id,result.claim_attempt.id);assert.equal(f.requests[1].claim_context.body_digest,result.claim_attempt.output.body_digest);
  assert.equal(result.claim_attempt.body_digest,hash('Aé🙂Z'));assert.deepEqual(result.decisions.map(d=>d.disposition),dispositions);
  assert.ok(result.decisions.every(d=>d.claim_attempt_id===result.claim_attempt.id&&d.judge_attempt_id===result.judge_attempt.id));
  assert.deepEqual(await f.run(task),result);assert.equal(f.requests.length,2);assert.deepEqual(await originals(f),before);assert.deepEqual(await semantic(f),[{count:0}]);
  assert.equal((await f.store.createExtractionJudgeTask({claim_task_id:task.id},context)).id,result.judge.id);
  const read=await f.store.readExtractionDecisions(result.judge_attempt.id,context);assert.deepEqual(read,result.decisions);
  const completion={id:result.judge_attempt.id,task_id:result.judge.id,expected_version:1,lease_epoch:result.judge_attempt.lease.epoch,state:'succeeded',reason:null,disposition:'unknown',output:result.judge_attempt.output,spans:result.judge_attempt.spans};
  assert.deepEqual(await f.store.recordExtractionAttempt(completion,context),result.judge_attempt);
  await assert.rejects(f.store.recordExtractionAttempt({...completion,state:'failed',reason:'provider_rejected',disposition:null,output:null,spans:[]},context),/attempt_conflict/);
  assert.equal((await f.store.readExtractionDecisions(result.judge_attempt.id,context)).length,4);
  audit('retained-pipeline',{result,requests:f.requests});
 }finally{await f.close();}
});

test('claim-only pipeline cannot advance audit coverage; decisions are atomic with judge terminal success',async()=>{
 const f=await setup();try{
  const task=await f.create();await f.engine.runExtractionTask(lease(task),context);
  const advance=partition=>f.store.recordExtractionCoverage({generation_id:f.g.id,partition,expected_covered_ingest_seq:0,covered_ingest_seq:1},context);
  await assert.rejects(advance('active_extraction'),/extraction_audit_incomplete/);
  const result=await f.run(task);await advance('active_extraction');await advance('episodes');
  assert.equal((await f.store.getExtractionGeneration(f.g.id,context)).covered_ingest_seq,1);
  const outbox=await f.query('MATCH (o:Outbox) RETURN o.processed_at AS processed');assert.ok(outbox.every(o=>o.processed===null));
  await assert.rejects(f.store.cutoverExtractionGeneration({generation_id:f.g.id,expected_generation_id:null,expected_selector_version:0},context),{code:'activation_prerequisite_unavailable'});
  audit('audit-only-coverage',{result,outbox});
 }finally{await f.close();}
});

test('unsupported judge ABI fails closed, explicit retry retains the failed attempt',async()=>{
 let supported=false;const f=await setup(async input=>input.task==='claim'?claims(input.text):supported?judgment(input):{task:'judge',disposition:'unknown',spans:[],language:'und',modality:'text'});
 try{
  const task=await f.create(),failed=await f.run(task);assert.equal(failed.judge.state,'failed');assert.equal(failed.judge_attempt.reason,'provider_mismatch');assert.equal(failed.judge_attempt.detail,'normalize');assert.deepEqual(failed.decisions,[]);
  supported=true;await f.store.retryModelTask({task_id:failed.judge.id,expected_version:failed.judge.version},context);
  const result=await f.run(task);assert.equal(result.judge.state,'succeeded');assert.notEqual(result.judge_attempt.id,failed.judge_attempt.id);
  assert.deepEqual(await f.store.getExtractionAttempt(failed.judge_attempt.id,context),failed.judge_attempt);assert.deepEqual(await semantic(f),[{count:0}]);audit('retry',{failed,result});
 }finally{await f.close();}
});

for(const count of [0,64])test(`exact ${count} claim boundary retains a complete bounded decision set`,async()=>{
 const f=await setup(async input=>input.task==='claim'?{...claims(input.text),claims:Array.from({length:count},()=>claims(input.text).claims[0])}:{...judgment(input),decisions:input.claim_context.claims.map((c,claim_index)=>({claim_index,disposition:'unknown',evidence:c.evidence}))});
 try{const result=await f.run(await f.create());assert.equal(result.judge.state,'succeeded');assert.equal(result.decisions.length,count);assert.deepEqual(await semantic(f),[{count:0}]);audit('decision-bound',{count,judge_attempt_id:result.judge_attempt.id});}finally{await f.close();}
});

test('missing disposition authority rejects reads and cannot seal coverage',async()=>{
 const f=await setup();try{const task=await f.create(),result=await f.run(task);
  await f.query('MATCH (d:ExtractionDisposition {judge_attempt_id:$id,claim_index:0}) DELETE d',{id:result.judge_attempt.id});
  await assert.rejects(f.store.readExtractionPipeline(task.id,context),/extraction_audit_conflict/);
  await assert.rejects(f.store.recordExtractionCoverage({generation_id:f.g.id,partition:'active_extraction',expected_covered_ingest_seq:0,covered_ingest_seq:1},context),/extraction_audit_conflict/);
 }finally{await f.close();}
});

for(const [corruption,detail] of [['digest','digest'],['order','judge_shape'],['span','judge_shape']])test(`judge ${corruption} mismatch retains failure without decisions, naming the failed check`,async()=>{
 const f=await setup(async input=>{const body=output(input);if(input.task==='judge_claims'){if(corruption==='digest')body.claim_body_digest='f'.repeat(64);if(corruption==='order')body.decisions.reverse();if(corruption==='span')body.decisions[0].evidence={start:0,end:1,text:'A'};}return body;});
 try{const result=await f.run(await f.create());assert.equal(result.judge_attempt.reason,'provider_mismatch');assert.equal(result.judge_attempt.detail,detail);assert.equal(result.judge_attempt.output,null);assert.deepEqual(result.decisions,[]);audit('rejected-judge-binding',{corruption,result});}finally{await f.close();}
});

for(const race of ['deny','head','policy','cancel','parent'])test(`in-flight judge ${race} is fenced without semantic writes`,async()=>{
 const f=await setup(async(input,events)=>{if(input.task==='claim')return output(input);const release=once(events,'release',{signal:AbortSignal.timeout(10000)});events.emit('entered');await release;return output(input);});
 let running;
 try{
  const task=await f.create(),entered=once(f.events,'entered',{signal:AbortSignal.timeout(10000)});
  running=f.run(task).then(value=>({value}),error=>({error}));await entered;
  const state=await f.store.readExtractionPipeline(task.id,context);
  if(race==='deny')await f.engine.setPolicy({policy_id:uuid(),selector:{episode_id:f.source.id},scope:'content'},context);
  if(race==='head')await f.engine.remember({...f.original,content:'new revision',source_revision:'v2',expected_previous_revision_key:state.claim.source_revision});
  if(race==='policy')await f.engine.setPolicy({policy_id:uuid(),selector:{source:'unrelated-source'},scope:'content'},context);
  if(race==='cancel')await f.store.cancelModelTask({task_id:state.judge.id,expected_version:state.judge.version},context);
  if(race==='parent'){
   // Deliberate retained-control corruption, not a production mutation API:
   // simulate a stale persisted parent binding while the real HTTP call is held.
   const rows=await f.query('MATCH (p:ExtractionJudgeInput {id:$id}) RETURN p.body AS body',{id:state.judge.attempt_id});
   const premise=JSON.parse(rows[0].body);premise.claim_context.attempt_id=uuid();
   await f.query('MATCH (p:ExtractionJudgeInput {id:$id}) SET p.body=$body',{id:state.judge.attempt_id,body:JSON.stringify(premise)});
  }
  f.events.emit('release');const result=await running;
  if(race==='deny')assert.match(String(result.error),/policy_denied/);
  if(race==='cancel')assert.match(String(result.error),/attempt_conflict/);
  if(race==='parent'){
   assert.equal(result.error.code,'extraction_audit_conflict');f.clock(state.judge.lease.expires_at);
   await f.store.settleModelTask({task_id:state.judge.id,expected_version:state.judge.version,lease_epoch:state.judge.lease.epoch,reason:'expired'},context);
  }
  const rows=await f.query('MATCH (a:ExtractionAttempt {task_id:$id}) RETURN a.body AS body',{id:state.judge.id});assert.equal(rows.length,1);
  const attempt=JSON.parse(rows[0].body);assert.equal(attempt.output,null);assert.equal(attempt.reason,race==='deny'?'policy_denied':['head','policy'].includes(race)?'premises_changed':race==='parent'?'expired':'cancelled');
  assert.deepEqual(await semantic(f),[{count:0}]);assert.deepEqual(await f.query('MATCH (d:ExtractionDisposition) RETURN count(d) AS count'),[{count:0}]);audit('race',{race,attempt});
 }finally{f.events.emit('release');if(running)await running;await f.close();}
});

test('worker replacement settles judge loss; restart resumes without repeating claim',async()=>{
 const f=await setup();let replacement;try{
  const task=await f.create();await f.engine.runExtractionTask(lease(task),context);const judge=await f.store.createExtractionJudgeTask({claim_task_id:task.id},context);
  const held=await f.store.leaseModelTask(lease(judge),context);
  replacement=new Engine({...options,objectsRoot:f.root,clock:()=>100,extractionProvider:new HttpExtractionProvider(f.config)});await replacement.claimWriterEpoch();
  await assert.rejects(f.store.extractionTaskInput(held.id,held.lease.epoch,context),/stale_writer_epoch/);
  const lost=await replacement.store.settleModelTask({task_id:held.id,expected_version:held.version,lease_epoch:held.lease.epoch,reason:'worker_lost'},context);
  await replacement.store.retryModelTask({task_id:lost.id,expected_version:lost.version},context);
  const result=await replacement.runExtractionPipeline(lease(task),context);assert.equal(result.judge.state,'succeeded');assert.equal(f.requests.filter(r=>r.task==='claim').length,1);
  assert.equal((await replacement.store.getExtractionAttempt(held.attempt_id,context)).state,'worker_lost');audit('worker-loss',{lost,result});
 }finally{await replacement?.close();await f.close();}
});

test('unknown status, malformed admission, racing child admission and policy reads fail closed',async()=>{
 const f=await setup();try{
  const id=uuid();assert.deepEqual(await f.store.readExtractionPipeline(id,context),{state:'unknown',pipeline_id:id});
  await assert.rejects(f.engine.createExtractionPipeline({id,generation_id:f.g.id,source_id:f.source.id}),/unauthenticated/);
  await assert.rejects(f.engine.createExtractionPipeline({id,generation_id:f.g.id,source_id:f.source.id,claims:[]},context));
  const task=await f.create();await f.engine.runExtractionTask(lease(task),context);
  const children=await Promise.all([f.store.createExtractionJudgeTask({claim_task_id:task.id},context),f.store.createExtractionJudgeTask({claim_task_id:task.id},context)]);assert.equal(children[0].id,children[1].id);
  await f.run(task);await f.engine.setPolicy({policy_id:uuid(),selector:{source:f.root},scope:'content'},context);
  await assert.rejects(f.store.readExtractionPipeline(task.id,context),/policy_denied/);assert.equal(f.requests.length,2);
 }finally{await f.close();}
});

function peer(path){const socket=connect(path);let buffer=Buffer.alloc(0),id=0;const waiters=[];
 socket.on('data',bytes=>{buffer=Buffer.concat([buffer,bytes]);while(buffer.length>=4&&buffer.length>=4+buffer.readUInt32BE()){const n=buffer.readUInt32BE(),reply=RpcResponse.parse(JSON.parse(buffer.subarray(4,4+n)));buffer=buffer.subarray(4+n);waiters.shift()?.resolve(reply);}});
 const reject=error=>{for(const w of waiters.splice(0))w.reject(error);};socket.on('error',reject);socket.on('close',()=>reject(new Error('socket closed')));
 return {socket,request(method,params={}){return new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('RPC deadline')),15000);waiters.push({resolve:v=>{clearTimeout(timer);resolve(v)},reject:e=>{clearTimeout(timer);reject(e)}});const body=Buffer.from(JSON.stringify({jsonrpc:'2.0',id:++id,method,params})),header=Buffer.alloc(4);header.writeUInt32BE(body.length);socket.write(Buffer.concat([header,body]));});}};
}

test('real daemon UDS authenticates audit boundaries and recovers UNKNOWN without semantic authority',async()=>{
 let corruption=null;
 const f=await setup(async(input,events)=>{
  const body=output(input);if(input.task==='claim')return body;
  if(corruption===null){const release=once(events,'release',{signal:AbortSignal.timeout(10000)});events.emit('entered');await release;}
  if(corruption==='partial')body.decisions.pop();
  if(corruption==='duplicate')body.decisions[1].claim_index=0;
  if(corruption==='out_of_range')body.decisions[3].claim_index=64;
  if(corruption==='reordered')body.decisions.reverse();
  return body;
 });const root=await mkdtemp('/tmp/g004-pipeline-uds-');let p;
 const child=spawn('node',[process.env.G004_DAEMON],{env:{...process.env,ANAMNESIS_NEO4J_URI:options.uri,ANAMNESIS_NEO4J_PASSWORD:options.password,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:'pipeline-token',ANAMNESIS_EXTRACTION_CONFIG:JSON.stringify(f.config)},stdio:['ignore','pipe','pipe']});
 const exited=once(child,'exit',{signal:AbortSignal.timeout(60000)});child.stderr.on('data',b=>process.stderr.write(b));const lines=createInterface({input:child.stdout});
 const ready=new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(new Error('readiness deadline')),30000);lines.on('line',line=>{if(JSON.parse(line).event==='listening'){clearTimeout(timer);resolve();}});child.once('error',e=>{clearTimeout(timer);reject(e)});child.once('exit',code=>{clearTimeout(timer);reject(new Error(`exit ${code}`))});});
 const ok=reply=>{assert.ok('result'in reply,JSON.stringify(reply));return reply.result;};
 try{
  await ready;p=peer(root+'/anamnesis.sock');const id=uuid();
  assert.equal((await p.request('extraction.audit.status',{pipeline_id:id})).error.data.code,'unauthenticated');
  const hello=ok(await p.request('hello',{version:1,client:'qa',token:'pipeline-token',commit_mode:'receipt'}));assert.equal(hello.capabilities.extraction,false);
  assert.deepEqual(ok(await p.request('extraction.audit.status',{pipeline_id:id})),{state:'unknown',pipeline_id:id});
  const task=ok(await p.request('extraction.audit.create',{id,generation_id:f.g.id,source_id:f.source.id}));
  const entered=once(f.events,'entered',{signal:AbortSignal.timeout(10000)});
  const pending=p.request('extraction.audit.run',lease(task)).then(value=>({value}),error=>({error}));
  await entered;p.socket.destroy();assert.match(String((await pending).error),/socket closed/);
  f.events.emit('release');
  p=peer(root+'/anamnesis.sock');ok(await p.request('hello',{version:1,client:'qa-new',token:'pipeline-token',commit_mode:'receipt'}));
  const result=ok(await p.request('extraction.audit.status',{pipeline_id:id}));assert.equal(result.judge.state,'succeeded');
  assert.deepEqual(ok(await p.request('extraction.audit.run',lease(task))),result);assert.equal(f.requests.length,2);
  for(const mode of ['partial','duplicate','out_of_range','reordered']){
   corruption=mode;
   const episode={schema:'anamnesis.original-message/1',content:'Aé🙂Z',mass:0.5,properties:{},time:f.original.time,origin:{...f.original.origin,record:mode}};
   const remembered=ok(await p.request('remember',{episode,source_revision:mode,expected_previous_revision_key:null}));
   const next=ok(await p.request('extraction.audit.create',{id:uuid(),generation_id:f.g.id,source_id:remembered.id}));
   const refused=ok(await p.request('extraction.audit.run',lease(next)));
   assert.equal(refused.judge_attempt.state,'failed');assert.equal(refused.judge_attempt.reason,'provider_mismatch');assert.equal(refused.judge_attempt.detail,mode==='out_of_range'?'normalize':'judge_shape');assert.equal(refused.judge_attempt.output,null);assert.deepEqual(refused.decisions,[]);
   assert.deepEqual(ok(await p.request('extraction.audit.status',{pipeline_id:next.id})),refused);
   audit('uds-decision-refusal',{mode,refused});
  }
  const denied=ok(await p.request('policy.set',{policy_id:uuid(),selector:{episode_id:f.source.id},scope:'content'}));
  assert.equal((await p.request('extraction.audit.status',{pipeline_id:id})).error.data.code,'policy_denied');
  assert.deepEqual(await semantic(f),[{count:0}]);audit('uds-audit',{hello,result,denied});
  ok(await p.request('shutdown'));assert.equal((await exited)[0],0);
 }finally{p?.socket.destroy();lines.close();if(child.exitCode===null&&child.signalCode===null){child.kill('SIGKILL');await exited;}await rm(root,{recursive:true,force:true});await f.close();audit('uds-cleanup',{root,removed:true,exit:child.exitCode,signal:child.signalCode});}
});
