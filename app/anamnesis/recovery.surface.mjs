// Real built-Node scenarios. The caller owns the isolated Neo4j container.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { EventEmitter, once } from 'node:events';
import { createServer, connect } from 'node:net';
import { createInterface } from 'node:readline';
import { createHash, randomUUID } from 'node:crypto';
import { appendFileSync, writeFileSync } from 'node:fs';
import { mkdtemp, readFile, rm, stat, readdir } from 'node:fs/promises';
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
const hash=bytes=>createHash('sha256').update(bytes).digest('hex');
const sha=value=>hash(JSON.stringify(value));
const key=p=>{const o=p.episode.origin;return sha([sha([o.source,o.session,o.actor,o.record]),p.source_revision]);};
const input=(record,content=`G002 ${record}`)=>({episode:{schema:'anamnesis.original-message/1',time:{value:'2026-09-09T00:00:00Z',precision:'second'},content,mass:1,properties:{},origin:{source:caseName,session:'s',actor:'a',record}},source_revision:'v1',expected_previous_revision_key:null});
async function rows(expected) {
  const result=await driver.executeQuery('MATCH (e:Episode) WHERE e.origin_source=$source RETURN e.id AS id, e.revision_key AS revision, e.ingest_seq AS seq, e.payload_hash AS payload ORDER BY seq',{source:caseName});
  const values=result.records.map(r=>r.toObject());
  assert.equal(values.length,expected);
  for(const field of ['id','revision','seq'])assert.equal(new Set(values.map(r=>r[field])).size,expected);
  checkpoint('database-exact-identities',{values});return values;
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
    assert.equal((await rows(1))[0].payload,payload_hash);checkpoint('uds-ingest',{first,duplicate,malformed:true,boundary:true});
  },
  async 'object-upload-crash'(){
    await start();let c=await client();const bytes=Buffer.from('incomplete upload');const sha256=hash(bytes);
    const begun=await c.request('object.begin',{sha256,size:bytes.length,media_type:'text/plain'});
    await c.request('object.chunk',{upload_id:begun.upload_id,seq:0,bytes_b64:bytes.subarray(0,3).toString('base64')});
    await kill();await start();c=await client();
    assert.deepEqual(await readdir(root+'/uploads'),[]);
    await assert.rejects(c.request('object.commit',{upload_id:begun.upload_id}),{code:'upload_not_found'});
    await assert.rejects(stat(`${root}/objects/${sha256.slice(0,2)}/${sha256}.json`),{code:'ENOENT'});
    const payload_hash=await upload(c);await kill();await start();c=await client();
    const receipt=await c.request('remember',{...input('durable'),payload_hash});
    assert.equal(receipt.created,true);assert.equal((await rows(1))[0].payload,payload_hash);
    checkpoint('object-upload-crash',{receipt,partialUploadAbsent:true});
  },
  async 'storage-retry'(){
    await start();const c=await client();
    const parent=input('parent');
    const child={...parent,source_revision:'v2',expected_previous_revision_key:key(parent)};
    await assert.rejects(c.request('remember',child),{code:'stale_revision'});
    offline();
    await assert.rejects(c.request('remember',parent),{code:'storage_unavailable',retryable:true});
    await assert.rejects(c.request('remember.batch',{deliveries:[parent,child]}),{code:'storage_unavailable',retryable:true});
    assert.equal((await c.request('status',{})).storage,'unavailable');
    await rows(0);online=true;
    const batch={deliveries:[parent,child]};
    const {results}=await c.request('remember.batch',batch);
    assert.deepEqual(results.map(r=>[r.revision_key,r.created]),[[key(parent),true],[key(child),true]]);
    assert.ok(results[0].ingest_seq<results[1].ingest_seq);
    assert.deepEqual((await c.request('remember.batch',batch)).results,results.map(r=>({...r,created:false})));
    await assert.rejects(c.request('remember',input('parent','changed')),{code:'revision_conflict'});
    await rows(2);checkpoint('storage-retry',{results});
  },
  async 'managed-ingest-restart'(){
    await start(true);let c=await client();const before=await c.request('status',{});const first=await c.request('remember',input('managed'));
    await closeClients();const owner=JSON.parse(await readFile(root+'/owner/owner.json','utf8'));pids.push(owner.pid);
    const restarted=nextEvent(daemon,e=>e.event==='listening');process.kill(owner.pid,'SIGKILL');await restarted;
    const replacement=JSON.parse(await readFile(root+'/owner/owner.json','utf8'));pids.push(replacement.pid);assert.notEqual(replacement.pid,owner.pid);
    c=await client();assert.deepEqual(await c.request('remember',input('managed')),{...first,created:false});const after=await c.request('status',{});assert.equal(after.data_incarnation,before.data_incarnation);assert.notEqual(after.fs_epoch,before.fs_epoch);
    const gate=await replyGate();let receipt;
    try{
      const proxy=await client(gate.path);const held=once(gate.signals,'held',{signal:AbortSignal.timeout(15000)});
      const pending=proxy.request('remember',input('lost-reply')).then(()=>null,error=>error);
      [receipt]=await held;assert.equal(receipt.state,'committed');gate.drop();const error=await pending;assert.equal(error?.code,'outcome_unknown');assert.equal(error?.retryable,false);
      const resolved=await c.request('remember',input('lost-reply'));assert.deepEqual(resolved,{...receipt,created:false});
      await assert.rejects(c.request('remember',input('lost-reply','different')),{code:'revision_conflict'});
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
