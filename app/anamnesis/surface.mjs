import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createServer, connect } from 'node:net';
import { EventEmitter, once } from 'node:events';
import { randomBytes, randomUUID, createHash } from 'node:crypto';
import { appendFileSync, writeFileSync } from 'node:fs';
import { mkdir, mkdtemp, readFile, rm, stat } from 'node:fs/promises';
import { createInterface } from 'node:readline';
import { resolve } from 'node:path';
import { RpcClient } from '../../dist/anamnesis-client.mjs';

const evidence = resolve('.omo/evidence/runtime-recovery/runtime-app-v3');
await mkdir(evidence,{recursive:true});
const owner=randomUUID();const name=`anamnesis-runtime-${owner}`;
const password=`qa-${randomBytes(18).toString('base64url')}`;
const token=randomBytes(32).toString('base64url');
const root=await mkdtemp('/tmp/ana-surface-');
const redact=text=>text.replaceAll(password,'[REDACTED]').replaceAll(token,'[REDACTED]');
const record=(file,value)=>writeFileSync(`${evidence}/${file}`,redact(JSON.stringify(value,null,2)+'\n'));
const append=(file,text)=>appendFileSync(`${evidence}/${file}`,redact(text));
record('surface-resources.json',{owner,name,root,pids:[],node:process.version});
const children=new Set();const clients=new Set();const attachments=[];const events=[];
const checkpoint=(name,value)=>{events.push({name,value});record('surface-events.json',events);console.log(name)};
function launch(command,args,{env=process.env,ready,timeout=120000,log='surface-process.txt'}={}) {
  append('surface-commands.jsonl',JSON.stringify({command,args})+'\n');
  const signals=new EventEmitter();
  const child=spawn(command,args,{env,stdio:['ignore','pipe','pipe']});children.add(child);
  record('surface-resources.json',{owner,name,root,pids:[...children].map(c=>c.pid),node:process.version});
  let output='';let markReady;let completed=false;
  const readyPromise=new Promise(resolve=>{markReady=resolve;});
  const timer=setTimeout(()=>{markReady(false);child.kill('SIGKILL');},timeout);
  const readers=[child.stdout,child.stderr].map(stream=>{
    stream.on('data',bytes=>{output+=bytes.toString();append(log,bytes.toString());});
    const lines=createInterface({input:stream,crlfDelay:Infinity});
    lines.on('line',line=>{
      if(ready?.test(line)){clearTimeout(timer);markReady(true);}
      let value;try{value=JSON.parse(line);}catch{return;}signals.emit(value.event,value);
    });return lines;
  });
  const done=new Promise((resolve,reject)=>{
    child.once('error',reject);
    child.once('close',(code,signal)=>{completed=true;clearTimeout(timer);markReady(false);for(const r of readers)r.close();children.delete(child);resolve({code,signal,output});});
  });
  return {child,done,signals,ready:readyPromise,async stop(signal='SIGKILL'){if(completed)return done;const guard=setTimeout(()=>child.kill('SIGKILL'),10000);try{child.kill(signal);return await done;}finally{clearTimeout(guard);}}};
}
async function command(executable,args,options){const result=await launch(executable,args,options).done;assert.equal(result.code,0,result.output);return result.output.trim();}
const docker=(args)=>command('docker',args,{env:{...process.env,NEO4J_AUTH:`neo4j/${password}`},log:'surface-docker.txt'});
const started=/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}(?:[+-]\d{4})?\s+INFO\s+Started\.$/;
let env;let daemon;let attempted=false;let failure;let targetPort=0;
const relaySockets=new Set();
const relay=createServer(socket=>{
  const upstream=connect(targetPort,'127.0.0.1');
  for(const s of [socket,upstream]){relaySockets.add(s);s.on('close',()=>relaySockets.delete(s));}
  socket.on('error',()=>upstream.destroy());upstream.on('error',()=>socket.destroy());
  socket.on('close',()=>upstream.destroy());upstream.on('close',()=>socket.destroy());
  socket.pipe(upstream);upstream.pipe(socket);
});
const listening=once(relay,'listening');relay.listen(0,'127.0.0.1');await listening;
const stablePort=relay.address().port;
async function startDatabase(){const attachment=launch('docker',['start','-a',name],{ready:started,timeout:180000,log:'surface-docker.txt'});attachments.push(attachment);if (!await attachment.ready) throw Error(`database did not start: ${(await attachment.done).output}`);targetPort=Number(await docker(['inspect','-f','{{(index (index .NetworkSettings.Ports "7687/tcp") 0).HostPort}}',name]));}
async function startDaemon(){daemon=launch(process.execPath,['dist/anamnesis-daemon.mjs'],{env,ready:/"event":"listening"/,timeout:120000});assert.equal(await daemon.ready,true,'daemon did not become ready');return daemon;}
async function client(){const c=await RpcClient.connect(root+'/anamnesis.sock',token);clients.add(c);return c;}
async function stopDaemon(){for(const c of clients){await c.close();}clients.clear();const stopped=await daemon.stop('SIGTERM');assert.equal(stopped.code,0,stopped.output);assert.match(stopped.output,/"event":"stopped"/);await assert.rejects(stat(root+'/anamnesis.sock'),{code:'ENOENT'});}
const input=(record)=>({episode:{schema:'anamnesis.original-message/1',time:{value:'2026-09-09T00:00:00Z',precision:'second'},content:`actual Node ${record}`,origin:{source:'surface',session:'session',actor:'operator',record},mass:1,properties:{language:'en'}},source_revision:'v1',expected_previous_revision_key:null});
const identity=result=>({revision_key:result.revision_key,body_digest:result.body_digest,data_incarnation:result.data_incarnation});
try {
  attempted=true;
  await docker(['create','--name',name,'--label',`anamnesis.qa.owner=${owner}`,'-p','127.0.0.1::7687','-e','NEO4J_AUTH','-e','NEO4J_server_memory_heap_max__size=512M','-e','NEO4J_server_memory_pagecache_size=256M','neo4j:5.26-community']);
  await startDatabase();
  env={...process.env,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:token,ANAMNESIS_NEO4J_URI:`bolt://127.0.0.1:${stablePort}`,ANAMNESIS_NEO4J_USER:'neo4j',ANAMNESIS_NEO4J_PASSWORD:password};
  record('surface-endpoint.json',{uri:env.ANAMNESIS_NEO4J_URI,name,targetPort,relay:'test-only stable TCP endpoint; Docker changes ephemeral published ports on restart'});
  await startDaemon();
  await assert.rejects(RpcClient.connect(root+'/anamnesis.sock','wrong-token'),{code:'authentication_failed'});checkpoint('wrong-token rejected',true);
  let c=await client();
  const status=await c.request('status',{});assert.equal(status.storage,'available');assert.equal(status.capabilities.writer_fence,'database');checkpoint('authenticated status',status);
  const contender=await launch(process.execPath,['dist/anamnesis-daemon.mjs'],{env,timeout:10000}).done;assert.equal(contender.code,1);assert.match(contender.output,/owned by live pid/);checkpoint('live owner protected',true);
  const bytes=Buffer.from('real object payload over UDS');const hash=createHash('sha256').update(bytes).digest('hex');
  const upload=await c.request('object.begin',{sha256:hash,size:bytes.length,media_type:'text/plain'});assert.equal(upload.state,'uploading');
  await assert.rejects(c.request('object.chunk',{upload_id:upload.upload_id,seq:1,bytes_b64:bytes.toString('base64')}),{code:'upload_sequence_mismatch'});
  await c.request('object.chunk',{upload_id:upload.upload_id,seq:0,bytes_b64:bytes.toString('base64')});
  assert.equal((await c.request('object.commit',{upload_id:upload.upload_id})).hash,hash);
  assert.equal((await c.request('object.begin',{sha256:hash,size:bytes.length,media_type:'text/plain'})).state,'committed');
  await assert.rejects(c.request('object.begin',{sha256:hash,size:bytes.length,media_type:'application/json'}),{code:'object_metadata_conflict'});
  const firstInput={...input('first'),payload_hash:hash};
  const first=await c.request('remember',firstInput);assert.equal(first.state,'committed');assert.equal(first.created,true);assert.equal(first.ingest_seq,1);
  const duplicate=await c.request('remember',firstInput);assert.equal(duplicate.id,first.id);assert.equal(duplicate.created,false);assert.equal(duplicate.ingest_seq,1);
  await assert.rejects(c.request('remember',{...firstInput,episode:{...firstInput.episode,content:'changed'}}),{code:'revision_conflict'});
  await assert.rejects(c.request('remember',{...firstInput,episode:{...firstInput.episode,mass:0.5}}),{code:'revision_conflict'});
  await assert.rejects(c.request('remember',{...firstInput,source_revision:'v2'}),{code:'stale_revision'});
  const revised=await c.request('remember',{...firstInput,source_revision:'v2',expected_previous_revision_key:first.revision_key});assert.equal(revised.ingest_seq,2);
  assert.equal((await c.request('ingest.status',identity(first))).state,'committed');
  assert.equal((await c.request('ingest.status',{...identity(first),body_digest:'0'.repeat(64)})).state,'unknown');
  checkpoint('upload remember duplicate conflict predecessor and exact status',{first,duplicate,revised});
  const cliStatus=JSON.parse(await command(process.execPath,['dist/anamnesis-ops.mjs','status'],{env}));assert.equal(cliStatus.state,'ready');
  const verify=JSON.parse(await command(process.execPath,['dist/anamnesis-ops.mjs','verify'],{env}));assert.equal(verify.ok,true);checkpoint('ops status and verify',verify);
  await docker(['stop','-t','10',name]);
  const queued=[];
  for(let i=0;i<50;i++){const params=input(`offline-${i}`);const receipt=await c.request('remember',params);assert.equal(receipt.state,'spooled');queued.push({params,receipt});}
  const degraded=await c.request('status',{});assert.equal(degraded.spool.pending,50);assert.equal(degraded.storage,'unavailable');
  checkpoint('DB stopped: 50 durably accepted records',{status:degraded,receipts:queued.map(x=>x.receipt)});
  await stopDaemon();
  await startDaemon();c=await client();
  const restarted=await c.request('status',{});assert.equal(restarted.data_incarnation,status.data_incarnation);assert.notEqual(restarted.fs_epoch,status.fs_epoch);assert.equal(restarted.spool.pending,50);
  assert.deepEqual(await c.request('remember',queued[0].params),queued[0].receipt);
  checkpoint('daemon restarted offline: receipts recovered',restarted);
  const settled=once(daemon.signals,'drain_settled',{signal:AbortSignal.timeout(120000)});
  await startDatabase();
  await c.request('status',{});await settled;
  const drained=await c.request('status',{});assert.equal(drained.storage,'available');assert.equal(drained.spool.pending,0);assert.equal(drained.outbox_pending,52);
  const committed=[];
  for(const item of queued){const result=await c.request('ingest.status',identity(item.receipt));assert.equal(result.state,'committed');committed.push(result);}
  assert.equal(new Set(committed.map(x=>x.ingest_seq)).size,50);checkpoint('DB restarted: all 50 deliveries drained through Engine',{status:drained,committed});
  await stopDaemon();
  await startDaemon();c=await client();assert.equal((await c.request('ingest.status',identity(first))).state,'committed');assert.equal((await c.request('status',{})).spool.pending,0);
  checkpoint('ordinary restart preserves committed delivery evidence',true);
  await c.request('shutdown',{});clients.delete(c);await c.close();
  const exited=await daemon.done;assert.equal(exited.code,0);assert.match(exited.output,/"event":"stopped"/);checkpoint('RPC shutdown completed',true);
  record('surface-result.json',{ok:true,events:events.length});
} catch(error){failure=error;record('surface-result.json',{ok:false,error:String(error),stack:error.stack});}
finally {
  const cleanup=[];
  try {
    for(const c of clients)await c.close();clients.clear();
    if(daemon)await daemon.stop();
    if(attempted){const inspected=JSON.parse(await docker(['inspect','--format','{{json .}}',name]));assert.equal(inspected.Config.Labels['anamnesis.qa.owner'],owner);await docker(['rm','-f','-v',inspected.Id]);cleanup.push({container:inspected.Id,removed:true});}
    for(const attachment of attachments)await attachment.stop();
    for(const child of children)child.kill('SIGKILL');
    for(const socket of relaySockets)socket.destroy();
    await new Promise((resolve,reject)=>relay.close(error=>error?reject(error):resolve()));
    cleanup.push({relay_closed:true});
    await rm(root,{recursive:true,force:true});cleanup.push({root,removed:true});
  } catch(error){failure=new AggregateError(failure?[failure,error]:[error],'surface cleanup failed');cleanup.push({error:String(error)});}
  record('surface-cleanup.json',cleanup);
}
if(failure)throw failure;
