import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { createServer, connect } from 'node:net';
import { createInterface } from 'node:readline';
import { createHash, randomBytes, randomUUID } from 'node:crypto';
import { mkdtemp, mkdir, readFile, rm, writeFile, appendFile } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
import neo4j from 'neo4j-driver';

const evidence=resolve(process.env.RUNTIME_PAGE_EVIDENCE_ROOT ?? '.omo/evidence/runtime-page-integration/owned-db');
await mkdir(evidence,{recursive:true});
const bundleRoot=resolve(process.env.RUNTIME_PAGE_BUNDLE_ROOT ?? '.omo/evidence/runtime-page-integration');
const {RpcClient}=await import(pathToFileURL(`${bundleRoot}/page-client.mjs`));
const owner=randomUUID(), name=`anamnesis-page-${owner}`;
const password=randomBytes(24).toString('base64url'), token=randomBytes(24).toString('base64url');
const root=await mkdtemp('/tmp/ana-page-db-');
const redact=text=>text.replaceAll(password,'[REDACTED]').replaceAll(token,'[REDACTED]');
const record=(file,data)=>writeFile(`${evidence}/${file}`,redact(JSON.stringify(data,null,2)+'\n'));
const children=new Set();let logs=Promise.resolve();
function launch(command,args,{env=process.env,ready,deadline=180000}={}) {
  const signals=new EventEmitter();
  const child=spawn(command,args,{env,stdio:['ignore','pipe','pipe']});children.add(child);
  let output='',markReady;const readyPromise=new Promise(resolve=>{markReady=resolve;});
  const timer=setTimeout(()=>{markReady(false);child.kill('SIGKILL');},deadline);
  const readers=[child.stdout,child.stderr].map(stream=>{
    stream.on('data',bytes=>{output+=bytes;logs=logs.then(()=>appendFile(`${evidence}/processes.txt`,redact(bytes.toString())));});
    const lines=createInterface({input:stream});lines.on('line',line=>{
      if(ready?.test(line)){clearTimeout(timer);markReady(true);}
      let value;try{value=JSON.parse(line);}catch{return;}signals.emit(value.event,value);
    });return lines;
  });
  const done=new Promise((resolve,reject)=>{
    child.once('error',reject);
    child.once('close',(code,signal)=>{clearTimeout(timer);markReady(false);children.delete(child);for(const reader of readers)reader.close();resolve({code,signal,output});});
  });
  return {child,done,ready:readyPromise,signals};
}
async function command(executable,args,options) {
  const result=await launch(executable,args,options).done;assert.equal(result.code,0,result.output);return result.output.trim();
}
const docker=args=>command('docker',args,{env:{...process.env,NEO4J_AUTH:`neo4j/${password}`}});
const sockets=new Set();let targetPort=1;
const relay=createServer(socket=>{
  const upstream=connect(targetPort,'127.0.0.1');
  for(const s of [socket,upstream]){sockets.add(s);s.on('close',()=>sockets.delete(s));}
  socket.on('error',()=>upstream.destroy());upstream.on('error',()=>socket.destroy());
  socket.on('close',()=>upstream.destroy());upstream.on('close',()=>socket.destroy());
  socket.pipe(upstream);upstream.pipe(socket);
});
const listening=once(relay,'listening');relay.listen(0,'127.0.0.1');await listening;
const sha=value=>createHash('sha256').update(JSON.stringify(value)).digest('hex');
const key=params=>{const o=params.episode.origin;return sha([sha([o.source,o.session,o.actor,o.record]),params.source_revision]);};
const input=(record,revision='v1',previous=null)=>({episode:{schema:'anamnesis.original-message/1',time:{value:'2026-09-09T00:00:00Z',precision:'second'},content:`${record}/${revision}`,mass:1,properties:{},origin:{source:'page-db',session:'s',actor:'a',record}},source_revision:revision,expected_previous_revision_key:previous});
const identity=receipt=>({revision_key:receipt.revision_key,body_digest:receipt.body_digest,data_incarnation:receipt.data_incarnation});
let attempted=false,attachment,daemon,client,driver,failure;
const results=[];
try {
  await record('resources.json',{owner,name,root});
  attempted=true;
  await docker(['create','--name',name,'--label',`anamnesis.qa.owner=${owner}`,'-p','127.0.0.1::7687','-e','NEO4J_AUTH','-e','NEO4J_server_memory_heap_max__size=512M','-e','NEO4J_server_memory_pagecache_size=256M','neo4j:5.26-community']);
  attachment=launch('docker',['start','-a',name],{ready:/^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}(?:[+-]\d{4})?\s+INFO\s+Started\.$/});
  assert.equal(await attachment.ready,true,'database did not emit Started');
  const dbPort=Number(await docker(['inspect','-f','{{(index (index .NetworkSettings.Ports "7687/tcp") 0).HostPort}}',name]));targetPort=dbPort;
  driver=neo4j.driver(`bolt://127.0.0.1:${dbPort}`,neo4j.auth.basic('neo4j',password),{disableLosslessIntegers:true});
  await driver.verifyConnectivity();
  daemon=launch(process.execPath,[`${bundleRoot}/page-daemon.mjs`],{env:{...process.env,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:token,ANAMNESIS_NEO4J_PASSWORD:password,ANAMNESIS_NEO4J_USER:'neo4j',ANAMNESIS_NEO4J_URI:`bolt://127.0.0.1:${relay.address().port}`},ready:/"event":"listening"/});
  assert.equal(await daemon.ready,true,'daemon did not become ready');
  client=await RpcClient.connect(root+'/anamnesis.sock',token);
  const staleCurrent=await client.request('remember',input('stale','current'));assert.equal(staleCurrent.state,'committed');
  // Disconnection is synchronous and observed by the next actual RPC; no sleeps.
  targetPort=1;for(const socket of sockets)socket.destroy();
  const missing=input('missing','absent');const a=input('missing','a',key(missing));
  const b=input('missing','b',key(a));const c=input('missing','c',key(b));
  const head=await client.request('remember',a);assert.equal(head.state,'spooled');
  const parent=input('chain','v1');const child=input('chain','v2',key(parent));
  const childReceipt=await client.request('remember',child);assert.equal(childReceipt.spool_seq,2);
  for(let i=0;i<98;i++)assert.equal((await client.request('remember',input(`filler-${i}`))).state,'spooled');
  const parentReceipt=await client.request('remember',parent);assert.equal(parentReceipt.spool_seq,101);
  const blocked=[[head,'missing_predecessor']];
  for(const params of [c,b])blocked.push([await client.request('remember',params),'missing_predecessor']);
  const ca=input('cycle','a');const cb=input('cycle','b',key(ca));ca.expected_previous_revision_key=key(cb);
  for(const [params,reason] of [[input('cycle','tail',key(ca)),'missing_predecessor'],[ca,'dependency_cycle'],[cb,'dependency_cycle']])blocked.push([await client.request('remember',params),reason]);
  const stale=input('stale','a');const staleChild=input('stale','b',key(stale));
  blocked.push([await client.request('remember',staleChild),'missing_predecessor']);
  blocked.push([await client.request('remember',stale),'stale_revision']);
  const settled=once(daemon.signals,'drain_settled',{signal:AbortSignal.timeout(120000)});
  targetPort=dbPort;
  assert.equal((await client.request('status',{})).storage,'available');await settled;
  const status=await client.request('status',{});assert.equal(status.storage,'available');assert.equal(status.spool.pending,108);assert.equal(status.spool.blocked,8);
  for(const [receipt,reason] of blocked)assert.equal((await client.request('ingest.status',identity(receipt))).reason,reason);
  const committedChild=await client.request('ingest.status',identity(childReceipt));assert.equal(committedChild.state,'committed');
  const committedParent=await client.request('ingest.status',identity(parentReceipt));assert.equal(committedParent.state,'committed');
  const verified=await driver.executeQuery('MATCH (h:OriginHead {origin_key:$origin}) MATCH (e:Episode {revision_key:$child}) RETURN h.revision_key AS head, e.previous_revision_key AS predecessor',{origin:sha(['page-db','s','a','chain']),child:key(child)});
  assert.equal(verified.records[0].get('head'),key(child));assert.equal(verified.records[0].get('predecessor'),key(parent));
  const beforeDone=await readFile(root+'/spool/spool.done');
  const done=JSON.parse(JSON.parse(beforeDone.toString()).payload);assert.equal(done.frontier,0);assert.deepEqual(done.completed,Array.from({length:100},(_,i)=>i+2));
  const repeat=await client.request('status',{});assert.equal(repeat.spool.blocked,8);assert.deepEqual(await readFile(root+'/spool/spool.done'),beforeDone);
  const absent=await driver.executeQuery('MATCH (e:Episode) WHERE e.revision_key IN $keys RETURN count(e) AS count',{keys:blocked.map(([receipt])=>receipt.revision_key)});assert.equal(absent.records[0].get('count'),0);
  const counts=await driver.executeQuery('MATCH (e:Episode) RETURN count(e) AS count, count(DISTINCT e.ingest_seq) AS sequences');assert.equal(counts.records[0].get('count'),101);assert.equal(counts.records[0].get('sequences'),101);
  await assert.rejects(client.request('remember',input('chain','bad',key(parent))),{code:'stale_revision'});
  // A post-drain control request proceeds; this is not a bound on total drain work.
  assert.equal((await client.request('shutdown',{})).state,'stopping');await client.close();client=undefined;
  assert.equal((await daemon.done).code,0);
  results.push({status,committedChild,committedParent,blocked:blocked.map(([receipt,reason])=>({sequence:receipt.spool_seq,reason})),sameOriginHeadVerified:true,blockedCompletionAbsent:true,controlRequestCompleted:true});
  await record('result.json',{ok:true,results});
} catch(error) {failure=error;await record('result.json',{ok:false,error:String(error),stack:error.stack});}
finally {
  const cleanup=[];
  try {
    if(client)await client.close();
    if(daemon && children.has(daemon.child)){daemon.child.kill('SIGKILL');await daemon.done;}
    if(driver)await driver.close();
    if(attempted){const inspected=JSON.parse(await docker(['inspect','--format','{{json .}}',name]));assert.equal(inspected.Config.Labels['anamnesis.qa.owner'],owner);await docker(['rm','-f','-v',inspected.Id]);cleanup.push({container:inspected.Id,removed:true});}
    if(attachment)await attachment.done;
    for(const socket of sockets)socket.destroy();await new Promise((resolve,reject)=>relay.close(error=>error?reject(error):resolve()));
    await rm(root,{recursive:true,force:true});cleanup.push({root,removed:true,relayClosed:true});
  } catch(error){failure=new AggregateError(failure?[failure,error]:[error],'cleanup failed');cleanup.push({error:String(error)});}
  await logs;await record('cleanup.json',cleanup);
}
if(failure)throw failure;
console.log('owned DB same-origin paging/CAS/classification passed; cleanup complete');
