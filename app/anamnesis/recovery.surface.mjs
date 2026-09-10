// Real built-Node scenarios. The caller owns the isolated Neo4j container.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { createServer, connect } from 'node:net';
import { createInterface } from 'node:readline';
import { createHash, randomUUID } from 'node:crypto';
import { appendFileSync, writeFileSync } from 'node:fs';
import { mkdtemp, readFile, writeFile, appendFile, rm, stat, readdir } from 'node:fs/promises';
import { resolve } from 'node:path';
import neo4j from 'neo4j-driver';
import { RpcClient } from '../../dist/anamnesis-client.mjs';

const [caseName, evidencePath] = process.argv.slice(2);
const evidence = resolve(evidencePath);
const uri = process.env.ANAMNESIS_TEST_NEO4J_URI;
const password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
assert.ok(uri && password, 'ownership-safe runner must supply isolated credentials');
const root = await mkdtemp('/tmp/ana-g002-');
const token = randomUUID();
const redact = text => text.replaceAll(password, '[REDACTED]').replaceAll(token, '[REDACTED]');
const record = (file, value) => writeFileSync(`${evidence}/${file}`, redact(JSON.stringify(value, null, 2)+'\n'));
const events = [];
const checkpoint = (name, value) => { events.push({name,value}); record('runtime-events.json',events); };
const processes = new Set(), clients = new Set(), connections = new Set(), extraServers = new Set();
const pids = [];
function launch(args, {ready = /"event":"listening"/, env: overrides = {}} = {}) {
  const child = spawn(process.execPath,args,{env:{...env,...overrides},stdio:['ignore','pipe','pipe']});
  const signals = new EventEmitter();
  let output='', settled=false, readyResolve;
  const readyPromise = new Promise(resolve=>{readyResolve=resolve;});
  const timer = setTimeout(()=>{readyResolve(false);child.kill('SIGKILL');},30000);
  const readers = [child.stdout,child.stderr].map(stream=>{
    stream.on('data',bytes=>{output+=bytes;appendFileSync(`${evidence}/runtime-processes.txt`,redact(bytes.toString()));});
    const reader=createInterface({input:stream});
    reader.on('line',line=>{
      if(ready.test(line)){clearTimeout(timer);readyResolve(true);}
      let event;try{event=JSON.parse(line);}catch{return;}
      signals.emit('event',event);
    });return reader;
  });
  const done = new Promise(resolve=>{
    child.once('error',error=>{output+=String(error);});
    child.once('close',(code,signal)=>{
      settled=true;clearTimeout(timer);readyResolve(false);processes.delete(handle);
      for(const reader of readers)reader.close();
      signals.emit('terminal');resolve({code,signal,output});
    });
  });
  const handle={child,done,ready:readyPromise,signals,async stop(signal='SIGKILL'){
    if(!settled){const timer=setTimeout(()=>child.kill('SIGKILL'),15000);child.kill(signal);try{return await done;}finally{clearTimeout(timer);}}
    return done;
  }};
  processes.add(handle);pids.push(child.pid);record('runtime-resources.json',{root,pids,caseName,node:process.version});
  return handle;
}
function nextEvent(handle,predicate) {
  return new Promise((resolve,reject)=>{
    const finish=(error,event)=>{clearTimeout(timer);handle.signals.off('event',observe);handle.signals.off('terminal',terminal);error?reject(error):resolve(event);};
    const observe=event=>{if(predicate(event))finish(null,event);};
    const terminal=()=>finish(Error('process ended before expected event'));
    const timer=setTimeout(()=>finish(Error('event deadline')),30000);
    handle.signals.on('event',observe);handle.signals.on('terminal',terminal);
  });
}
let online=true;
function track(socket) { connections.add(socket);socket.once('close',()=>connections.delete(socket));return socket; }
const endpoint=new URL(uri);
const relay=createServer(socket=>{
  track(socket);
  if(!online){socket.destroy();return;}
  const upstream=track(connect(Number(endpoint.port),endpoint.hostname));
  socket.on('error',()=>upstream.destroy());upstream.on('error',()=>socket.destroy());
  socket.on('close',()=>upstream.destroy());upstream.on('close',()=>socket.destroy());
  socket.pipe(upstream);upstream.pipe(socket);
});
const listening=once(relay,'listening',{signal:AbortSignal.timeout(5000)});relay.listen(0,'127.0.0.1');await listening;
const env={...process.env,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:token,ANAMNESIS_NEO4J_URI:`bolt://127.0.0.1:${relay.address().port}`,ANAMNESIS_NEO4J_USER:'neo4j',ANAMNESIS_NEO4J_PASSWORD:password};
const driver=neo4j.driver(uri,neo4j.auth.basic('neo4j',password),{disableLosslessIntegers:true,connectionTimeout:5000,maxTransactionRetryTime:0});
let daemon;
async function start(managed=false) {
  daemon=launch(managed?['dist/anamnesis-ops.mjs','managed']:[process.env.RUNTIME_DAEMON_ENTRY ?? 'dist/anamnesis-daemon.mjs']);
  assert.equal(await daemon.ready,true,(await Promise.race([daemon.done,Promise.resolve({output:'daemon not ready'})])).output);
  return daemon;
}
async function client(path=root+'/anamnesis.sock') {const c=await RpcClient.connect(path,token);clients.add(c);return c;}
async function closeClients(){for(const c of clients)await c.close();clients.clear();}
async function kill(){await closeClients();assert.equal((await daemon.stop()).signal,'SIGKILL');}
function offline(){online=false;for(const socket of connections)socket.destroy();}
async function recoverDrain(c) {
  const settled=nextEvent(daemon,e=>e.event==='drain_settled');
  online=true;await c.request('status',{});await settled;
  return c.request('status',{});
}
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const sha=value=>hash(JSON.stringify(value));
const key=p=>{const o=p.episode.origin;return sha([sha([o.source,o.session,o.actor,o.record]),p.source_revision]);};
const input=(record,content=`G002 ${record}`)=>({episode:{schema:'anamnesis.original-message/1',time:{value:'2026-09-09T00:00:00Z',precision:'second'},content,mass:1,properties:{},origin:{source:caseName,session:'s',actor:'a',record}},source_revision:'v1',expected_previous_revision_key:null});
const identity=r=>({revision_key:r.revision_key,body_digest:r.body_digest,data_incarnation:r.data_incarnation});
async function rows(expected) {
  const result=await driver.executeQuery('MATCH (e:Episode) WHERE e.origin_source=$source RETURN e.id AS id, e.revision_key AS revision, e.ingest_seq AS seq, e.payload_hash AS payload ORDER BY seq',{source:caseName});
  const values=result.records.map(r=>r.toObject());
  assert.equal(values.length,expected);
  for(const field of ['id','revision','seq'])assert.equal(new Set(values.map(r=>r[field])).size,expected);
  const outbox=await driver.executeQuery('MATCH (e:Episode) WHERE e.origin_source=$source OPTIONAL MATCH (o:Outbox {element_id:e.id}) RETURN e.id AS id, count(o) AS effects',{source:caseName});
  assert.equal(outbox.records.length,expected);for(const row of outbox.records)assert.equal(row.get('effects'),1);
  checkpoint('database-exact-identities',{values,outbox:outbox.records.map(r=>r.toObject())});return values;
}
async function upload(c,bytes=Buffer.from('G002 object payload')) {
  const sha256=hash(bytes), params={sha256,size:bytes.length,media_type:'text/plain'};
  const begin=await c.request('object.begin',params);assert.equal(begin.state,'uploading');
  await assert.rejects(c.request('object.chunk',{upload_id:begin.upload_id,seq:1,bytes_b64:bytes.toString('base64')}),{code:'upload_sequence_mismatch'});
  await c.request('object.chunk',{upload_id:begin.upload_id,seq:0,bytes_b64:bytes.toString('base64')});
  assert.equal((await c.request('object.commit',{upload_id:begin.upload_id})).hash,sha256);
  assert.equal(hash(await readFile(`${root}/objects/${sha256.slice(0,2)}/${sha256}`)),sha256);
  assert.equal((await c.request('object.begin',params)).state,'committed');
  checkpoint('object-hash',{sha256,bytes:bytes.length});return sha256;
}
function frame(value){const bytes=Buffer.from(typeof value==='string'?value:JSON.stringify(value));const f=Buffer.alloc(4+bytes.length);f.writeUInt32BE(bytes.length);bytes.copy(f,4);return f;}
async function raw(bytes) {
  const socket=track(connect(root+'/anamnesis.sock'));let buffer=Buffer.alloc(0);
  try{return await new Promise((resolve,reject)=>{
    const timer=setTimeout(()=>reject(Error('raw frame deadline')),5000);
    const finish=(error,value)=>{clearTimeout(timer);error?reject(error):resolve(value);};
    socket.once('error',e=>finish(e));
    socket.on('data',chunk=>{buffer=Buffer.concat([buffer,chunk]);if(buffer.length>=4&&buffer.length>=4+buffer.readUInt32BE())finish(null,JSON.parse(buffer.subarray(4,4+buffer.readUInt32BE()).toString()));});
    socket.write(bytes);
  });}finally{socket.destroy();}
}
// Drops exactly the first remember reply after the real daemon has produced it.
// Subscriptions precede action; no fake success, timers, polling or DB mocks.
async function replyGate() {
  const signals=new EventEmitter();let held;const sockets=new Set();
  const server=createServer(down=>{
    sockets.add(down);const up=connect(root+'/anamnesis.sock');sockets.add(up);
    for(const [s,other] of [[down,up],[up,down]]){s.on('error',()=>other.destroy());s.on('close',()=>{sockets.delete(s);other.destroy();});}
    down.pipe(up);let buffer=Buffer.alloc(0);
    up.on('data',bytes=>{
      buffer=Buffer.concat([buffer,bytes]);
      while(buffer.length>=4&&buffer.length>=4+buffer.readUInt32BE()){
        const n=4+buffer.readUInt32BE(), bytes=buffer.subarray(0,n);buffer=buffer.subarray(n);const response=JSON.parse(bytes.subarray(4).toString());
        if(response.method==='remember'&&!held){held={down,up,response};signals.emit('held',response.result);}
        else down.write(bytes);
      }
    });
  });
  const path=root+'/gate.sock';const ready=once(server,'listening',{signal:AbortSignal.timeout(5000)});server.listen(path);await ready;
  extraServers.add(server);
  return {path,signals,drop(){held.down.destroy();held.up.destroy();},async close(){for(const socket of sockets)socket.destroy();await new Promise((resolve,reject)=>server.close(e=>e?reject(e):resolve()));extraServers.delete(server);}};
}
const scenarios={
  async 'uds-ingest'(){
    await start();const c=await client();
    await assert.rejects(RpcClient.connect(root+'/anamnesis.sock','wrong'),{code:'authentication_failed'});
    assert.equal((await raw(frame('{'))).error.data.code,'parse_error');
    const oversize=Buffer.alloc(4);oversize.writeUInt32BE(1024*1024+1);assert.equal((await raw(oversize)).error.data.code,'invalid_request');
    // Raw malformed params are tested at daemon boundary, not rejected by client.
    assert.equal((await raw(frame({jsonrpc:'2.0',id:1,method:'remember',params:input('oversize','x'.repeat(65537))}))).error.data.code,'invalid_params');
    const payload_hash=await upload(c);const p={...input('one'),payload_hash};
    const first=await c.request('remember',p);assert.equal(first.state,'committed');assert.equal(first.revision_key,key(p));
    const duplicate=await c.request('remember',p);assert.equal(duplicate.id,first.id);assert.equal(duplicate.ingest_seq,first.ingest_seq);assert.equal(duplicate.created,false);
    await assert.rejects(c.request('remember',{...p,episode:{...p.episode,content:'conflict'}}),{code:'revision_conflict'});
    assert.equal((await c.request('ingest.status',{...identity(first),body_digest:'0'.repeat(64)})).state,'unknown');
    assert.equal((await rows(1))[0].payload,payload_hash);checkpoint('uds-ingest',{first,duplicate,malformed:true,boundary:true,unknown:true});
  },
  async 'object-spool-crashes'(){
    await start();let c=await client();const bytes=Buffer.from('incomplete upload');const sha256=hash(bytes);
    const begun=await c.request('object.begin',{sha256,size:bytes.length,media_type:'text/plain'});
    await c.request('object.chunk',{upload_id:begun.upload_id,seq:0,bytes_b64:bytes.subarray(0,3).toString('base64')});
    await kill();await start();c=await client();
    assert.deepEqual(await readdir(root+'/uploads'),[]);
    await assert.rejects(c.request('object.commit',{upload_id:begun.upload_id}),{code:'upload_not_found'});
    await assert.rejects(stat(`${root}/objects/${sha256.slice(0,2)}/${sha256}.json`),{code:'ENOENT'});
    const payload_hash=await upload(c);offline();const p={...input('durable'),payload_hash};const receipt=await c.request('remember',p);assert.equal(receipt.state,'spooled');
    await kill();const journal=root+'/spool/spool.journal';const before=await readFile(journal);await appendFile(journal,Buffer.from([0,0,0]));
    await start();c=await client();assert.deepEqual(await c.request('remember',p),receipt);assert.deepEqual(await readFile(journal),before);
    assert.equal((await recoverDrain(c)).spool.pending,0);assert.equal((await c.request('ingest.status',identity(receipt))).state,'committed');await rows(1);
    offline();const corrupt=await c.request('remember',input('corrupt'));await kill();
    const data=await readFile(journal);data[data.length-1]^=1;await writeFile(journal,data);
    await start();c=await client();assert.equal((await c.request('status',{})).spool.quarantined,1);
    assert.equal((await c.request('ingest.status',identity(corrupt))).state,'quarantined');
    await assert.rejects(c.request('remember',input('after-corruption')),{code:'spool_corrupt'});
    checkpoint('object-spool-crashes',{receipt,corrupt,partialUploadAbsent:true,tornSuffixTruncated:true,quarantined:true});
  },
  async 'outage-drain-50'(){
    await start();let c=await client();const before=await c.request('status',{});offline();const queued=[];
    for(let i=0;i<50;i++){const params=input(`offline-${i}`);const receipt=await c.request('remember',params);assert.equal(receipt.state,'spooled');assert.equal(receipt.spool_seq,i+1);queued.push({params,receipt});}
    assert.equal((await c.request('status',{})).spool.pending,50);await kill();await start();c=await client();
    const after=await c.request('status',{});assert.equal(after.spool.pending,50);assert.equal(after.data_incarnation,before.data_incarnation);assert.notEqual(after.fs_epoch,before.fs_epoch);
    for(const q of queued)assert.deepEqual(await c.request('remember',q.params),q.receipt);
    const drained=await recoverDrain(c);assert.equal(drained.spool.pending,0);assert.equal(drained.outbox_pending,50);
    const committed=[];for(const q of queued){const result=await c.request('ingest.status',identity(q.receipt));assert.equal(result.state,'committed');const retry=await c.request('remember',q.params);assert.equal(retry.id,result.id);assert.equal(retry.ingest_seq,result.ingest_seq);assert.equal(retry.created,false);committed.push(result);}
    const actual=await rows(50);assert.deepEqual(actual.map(r=>r.revision).sort(),queued.map(q=>key(q.params)).sort());
    assert.equal((await c.request('status',{})).outbox_pending,50);checkpoint('outage-drain-50',{before,after,drained,queued,committed});
  },
  async 'source-resume'(){
    await start();const source=root+'/source.jsonl', cp=root+'/checkpoint.json';
    const records=Array.from({length:3},(_,i)=>input(`source-${i}`));const text=records.map(JSON.stringify).join('\n')+'\n';await writeFile(source,text);
    const pendingPath=cp+'.pending.json';
    const runSource=(checkpoint=cp)=>launch(['dist/anamnesis-ops.mjs','ingest',source,checkpoint],{ready:/"event":"source_complete"/}).done;
    const failureCode=result=>JSON.parse(result.output.trim().split('\n').at(-1)).code;
    offline();
    const admitted=await runSource();assert.equal(admitted.code,1);assert.equal(failureCode(admitted),'source_pending_spooled');assert.doesNotMatch(admitted.output,/"event":"source_complete"/);
    const before=await readFile(cp), pendingBytes=await readFile(pendingPath), work=JSON.parse(pendingBytes);
    assert.equal(JSON.parse(before).next,0);assert.equal(JSON.parse(before).last,null);assert.deepEqual(work.params,records[0]);assert.equal(work.index,0);assert.equal(work.identity.revision_key,key(records[0]));assert.equal(work.source_hash,hash(text));
    let c=await client();const receipt=await c.request('ingest.status',work.identity);assert.equal(receipt.state,'spooled');assert.equal(receipt.body_digest,work.identity.body_digest);await rows(0);
    const stillOffline=await runSource();assert.equal(failureCode(stillOffline),'source_pending_spooled');assert.deepEqual(await readFile(cp),before);assert.deepEqual(await readFile(pendingPath),pendingBytes);
    await kill();await start();c=await client();assert.equal((await c.request('status',{})).spool.pending,1);
    const restartedOffline=await runSource();assert.equal(failureCode(restartedOffline),'source_pending_spooled');assert.deepEqual(await readFile(cp),before);assert.deepEqual(await readFile(pendingPath),pendingBytes);
    await recoverDrain(c);
    const drained=await runSource();assert.equal(drained.code,0,drained.output);assert.match(drained.output,/"event":"source_reconciled","index":0,"state":"committed"/);
    assert.equal(JSON.parse(await readFile(cp,'utf8')).next,3);await assert.rejects(stat(pendingPath),{code:'ENOENT'});await rows(3);
    checkpoint('source-offline-commit-order',{receipt,pending:work,offlineCheckpoint:JSON.parse(before),offlineRestartUnchanged:true,reconciledViaStatus:true,pendingRetired:true});
    // Kill only after the exact real COMMIT reply has reached the relay.
    const crashCp=root+'/crash-cp';
    const gate=await replyGate();
    try{
      const held=once(gate.signals,'held',{signal:AbortSignal.timeout(15000)});
      const ingest=launch(['dist/anamnesis-ops.mjs','ingest',source,crashCp],{env:{ANAMNESIS_RUNTIME_SOCKET:gate.path},ready:/"event":"source_complete"/});
      // An absent source command fails immediately, not by event deadline.
      const outcome=await Promise.race([held.then(([receipt])=>({receipt})),ingest.done.then(result=>({result}))]);
      assert.ok(outcome.receipt,JSON.stringify(outcome));assert.equal(outcome.receipt.state,'committed');
      const saved=JSON.parse(await readFile(crashCp,'utf8'));assert.equal(saved.next,0);
      assert.deepEqual(JSON.parse(await readFile(crashCp+'.pending.json','utf8')).identity,identity(outcome.receipt));
      await ingest.stop();gate.drop();
    }finally{await gate.close();}
    const resumed=await runSource(crashCp);assert.equal(resumed.code,0,resumed.output);assert.match(resumed.output,/"event":"source_reconciled"/);await assert.rejects(stat(crashCp+'.pending.json'),{code:'ENOENT'});
    const saved=JSON.parse(await readFile(cp,'utf8'));assert.equal(saved.next,3);
    assert.equal((await launch(['dist/anamnesis-ops.mjs','ingest',source,cp],{ready:/"event":"source_complete"/}).done).code,0);
    await rows(3);
    const unknownCheckpoint={...saved,last:{...saved.last,body_digest:'0'.repeat(64)}};
    await writeFile(cp,JSON.stringify(unknownCheckpoint));const unknown=await runSource();assert.equal(unknown.code,1);assert.match(unknown.output,/source_checkpoint_invalid/);assert.deepEqual(JSON.parse(await readFile(cp,'utf8')),unknownCheckpoint);await writeFile(cp,JSON.stringify(saved));
    const earlier=await c.request('remember',records[1]);assert.equal(earlier.state,'committed');assert.equal(earlier.created,false);
    for(const forged of [{...saved,next:2},{...saved,last:identity(earlier)}]){
      await writeFile(cp,JSON.stringify(forged));const bytes=await readFile(cp);const rejected=await runSource();assert.equal(rejected.code,1);assert.match(rejected.output,/source_checkpoint_invalid/);assert.deepEqual(await readFile(cp),bytes);await assert.rejects(stat(pendingPath),{code:'ENOENT'});
    }
    await writeFile(cp,JSON.stringify(saved));
    // Drop a replay reply without killing the source: UNKNOWN is surfaced and
    // its cursor remains zero, even though the real DB committed this identity.
    const lossGate=await replyGate();
    try{
      const held=once(lossGate.signals,'held',{signal:AbortSignal.timeout(15000)});
      const losing=launch(['dist/anamnesis-ops.mjs','ingest',source,root+'/loss-cp'],{env:{ANAMNESIS_RUNTIME_SOCKET:lossGate.path}});
      await held;lossGate.drop();const failed=await losing.done;assert.equal(failed.code,1);assert.match(failed.output,/outcome_unknown/);assert.equal(JSON.parse(await readFile(root+'/loss-cp','utf8')).next,0);
    }finally{await lossGate.close();}
    const lossCp=root+'/loss-cp', lossPending=lossCp+'.pending.json';
    const lostWork=JSON.parse(await readFile(lossPending,'utf8'));
    const lossResume=await runSource(lossCp);assert.equal(lossResume.code,0,lossResume.output);assert.match(lossResume.output,/"event":"source_reconciled"/);
    // A valid never-delivered identity must stay UNKNOWN; pending is not an
    // authorization to replay an unobserved request automatically.
    const unknownSource=root+'/unknown.jsonl', unknownCp=root+'/unknown-cp';
    const absent=input('never-delivered'), absentText=JSON.stringify(absent)+'\n';await writeFile(unknownSource,absentText);
    const keys=new Set();const collect=value=>{if(value&&typeof value==='object')for(const [k,v]of Object.entries(value)){if(!Array.isArray(value))keys.add(k);collect(v);}};
    const envelope={digest_version:1,params:absent};collect(envelope);
    const unknownIdentity={revision_key:key(absent),body_digest:hash(JSON.stringify(envelope,[...keys].sort())),data_incarnation:saved.data_incarnation};
    const unknownCpValue={...saved,next:0,last:null,source_hash:hash(absentText)};
    await writeFile(unknownCp,JSON.stringify(unknownCpValue));await writeFile(unknownCp+'.pending.json',JSON.stringify({...lostWork,source_hash:hash(absentText),params:absent,identity:unknownIdentity}));
    const unknownBefore=await readFile(unknownCp), unknownPendingBefore=await readFile(unknownCp+'.pending.json');
    const unresolved=await launch(['dist/anamnesis-ops.mjs','ingest',unknownSource,unknownCp]).done;assert.equal(unresolved.code,1);assert.equal(failureCode(unresolved),'source_pending_unknown');assert.deepEqual(await readFile(unknownCp),unknownBefore);assert.deepEqual(await readFile(unknownCp+'.pending.json'),unknownPendingBefore);assert.equal((await c.request('ingest.status',unknownIdentity)).state,'unknown');await rows(3);
    const unchangedCheckpoint=await readFile(cp);
    await writeFile(source,text+'{}\n');const changed=await runSource();assert.equal(changed.code,1);assert.match(changed.output,/source_changed/);assert.deepEqual(await readFile(cp),unchangedCheckpoint);await writeFile(source,text);
    await writeFile(cp,JSON.stringify({...saved,data_incarnation:randomUUID()}));const foreignBytes=await readFile(cp);const foreign=await runSource();assert.equal(foreign.code,1);assert.match(foreign.output,/incarnation_mismatch/);assert.deepEqual(await readFile(cp),foreignBytes);await writeFile(cp,JSON.stringify(saved));
    await writeFile(root+'/bad.jsonl','{\n');const bad=await launch(['dist/anamnesis-ops.mjs','ingest',root+'/bad.jsonl',root+'/bad-cp']).done;assert.equal(bad.code,1);await rows(3);
    await writeFile(root+'/oversize.jsonl',Buffer.alloc(16*1024*1024+1,32));const large=await launch(['dist/anamnesis-ops.mjs','ingest',root+'/oversize.jsonl',root+'/large-cp']).done;assert.equal(large.code,1);assert.match(large.output,/source_too_large/);await assert.rejects(stat(root+'/large-cp'),{code:'ENOENT'});
    checkpoint('source-resume',{saved,crashBeforeAcknowledgement:true,changedSourceRejectedUnchanged:true,foreignIncarnationRejectedUnchanged:true,wrongValidLastRejectedUnchanged:true,forgedNextRejectedUnchanged:true,malformedRejected:true,unknownPendingUnchangedWithoutReplay:true,commitBeforeCheckpoint:true,boundedSnapshot:true});
  },
  async 'normalized-agentlog'(){
    await start();let c=await client();
    const source=root+'/normalized-agentlog.jsonl', cp=root+'/normalized-agentlog.checkpoint.json';
    const occurred_at=Date.parse('2026-09-09T00:00:05Z');
    const event={provider:caseName,partition_id:'normalized-session',upstream_event_id:'event-1',occurred_at,role:'user',canonical_kind:'agent_message',kind:'message',text:'sealed normalized agentlog event'};
    await writeFile(source,JSON.stringify(event)+'\n');
    const run=()=>launch(['dist/anamnesis-ops.mjs','ingest-agentlog',root,cp],{ready:/'event":"source_complete"/}).done;
    const first=await run();assert.equal(first.code,0,first.output);assert.match(first.output,/"event":"source_scope"/);assert.match(first.output,/"event":"source_complete"/);
    const saved=JSON.parse(await readFile(cp,'utf8'));assert.equal(saved.next,1);assert.equal(saved.last.revision_key.length,64);
    await kill();await start();c=await client();
    const duplicate=await run();assert.equal(duplicate.code,0,duplicate.output);assert.equal(JSON.parse(await readFile(cp,'utf8')).next,1);
    const revision=createHash('sha256').update(new Date(occurred_at).toISOString()+'\n'+event.text).digest('hex');
    const conflict={episode:{schema:'anamnesis.original-message/1',time:{value:new Date(occurred_at).toISOString(),precision:'second'},content:'conflicting content',origin:{source:caseName,session:'normalized-session',actor:'user',record:'event-1'},properties:{}},source_revision:revision,expected_previous_revision_key:null};
    await assert.rejects(c.request('remember',conflict),{code:'revision_conflict'});
    const unknown={...saved,last:{...saved.last,body_digest:'0'.repeat(64)}};await writeFile(cp,JSON.stringify(unknown));
    const refused=await run();assert.equal(refused.code,1);assert.match(refused.output,/source_checkpoint_invalid/);assert.deepEqual(JSON.parse(await readFile(cp,'utf8')),unknown);
    await writeFile(cp,JSON.stringify(saved));await assert.rejects(stat(cp+'.pending.json'),{code:'ENOENT'});
    const db=await rows(1);checkpoint('normalized-agentlog',{sealed:true,first:JSON.parse(first.output.split('\n').find(line=>line.includes('source_complete'))),duplicate:true,conflict:true,restart:true,unknown:true,checkpoint:saved,episodes:db.length,ownedCleanup:true});
  },
  async 'managed-ingest-restart'(){
    await start(true);let c=await client();const before=await c.request('status',{});const first=await c.request('remember',input('managed'));
    await closeClients();const owner=JSON.parse(await readFile(root+'/owner/owner.json','utf8'));pids.push(owner.pid);
    const restarted=nextEvent(daemon,e=>e.event==='listening');process.kill(owner.pid,'SIGKILL');await restarted;
    const replacement=JSON.parse(await readFile(root+'/owner/owner.json','utf8'));pids.push(replacement.pid);assert.notEqual(replacement.pid,owner.pid);
    c=await client();assert.equal((await c.request('ingest.status',identity(first))).id,first.id);const after=await c.request('status',{});assert.equal(after.data_incarnation,before.data_incarnation);assert.notEqual(after.fs_epoch,before.fs_epoch);
    const gate=await replyGate();let receipt;
    try{
      const proxy=await client(gate.path);const held=once(gate.signals,'held',{signal:AbortSignal.timeout(15000)});
      const pending=proxy.request('remember',input('lost-reply')).then(()=>null,error=>error);
      [receipt]=await held;assert.equal(receipt.state,'committed');gate.drop();const error=await pending;assert.equal(error?.code,'outcome_unknown');assert.equal(error?.retryable,false);
      const resolved=await c.request('ingest.status',identity(receipt));assert.equal(resolved.id,receipt.id);
      assert.equal((await c.request('ingest.status',{...identity(receipt),body_digest:'0'.repeat(64)})).state,'unknown');
    }finally{await gate.close();}
    await rows(2);await closeClients();assert.equal((await daemon.stop('SIGTERM')).code,0);
    assert.throws(()=>process.kill(replacement.pid,0),{code:'ESRCH'});await assert.rejects(stat(root+'/owner'),{code:'ENOENT'});
    checkpoint('managed-ingest-restart',{owner:owner.pid,replacement:replacement.pid,first,receipt,unknownThenResolved:true,ownedChildStopped:true});
  },
};
let failure;
try{
  const bundles={};for(const path of [process.env.RUNTIME_DAEMON_ENTRY ?? 'dist/anamnesis-daemon.mjs','dist/anamnesis-client.mjs','dist/anamnesis-ops.mjs'])bundles[resolve(path)]=hash(await readFile(path));record('runtime-bundles.json',bundles);
  assert.ok(Object.hasOwn(scenarios,caseName),'unknown recovery scenario');await driver.verifyConnectivity();await scenarios[caseName]();record('runtime-result.json',{ok:true,caseName,events:events.length});
}catch(error){failure=error;record('runtime-result.json',{ok:false,caseName,error:String(error),stack:error.stack});}
finally{
  const cleanup=[];
  const attempt=async(name,work)=>{try{await work();cleanup.push({name,ok:true});}catch(error){cleanup.push({name,ok:false,error:String(error)});failure=new AggregateError(failure?[failure,error]:[error],`${name} cleanup failed`);}};
  await attempt('clients',closeClients);
  await attempt('processes',async()=>{for(const p of processes)await p.stop('SIGTERM');for(const pid of pids)assert.throws(()=>process.kill(pid,0),{code:'ESRCH'});});
  await attempt('database-driver',()=>driver.close());
  await attempt('relays',async()=>{for(const socket of connections)socket.destroy();for(const server of [relay,...extraServers])await new Promise((resolve,reject)=>server.close(e=>e?reject(e):resolve()));});
  await attempt('root',async()=>{await rm(root,{recursive:true,force:true});await assert.rejects(stat(root),{code:'ENOENT'});});
  record('runtime-resources.json',{root,pids,caseName,node:process.version});record('runtime-cleanup.json',cleanup);
}
if(failure)throw failure;
console.log(JSON.stringify({event:'scenario_complete',caseName}));
