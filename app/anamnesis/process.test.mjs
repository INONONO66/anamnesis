import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { mkdtemp, rm, stat, readFile, writeFile, readdir } from 'node:fs/promises';
import { connect } from 'node:net';
import { once } from 'node:events';
import { createHash } from 'node:crypto';
import { resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const entry = process.env.RUNTIME_ENTRY
  ? resolve(process.env.RUNTIME_ENTRY)
  : fileURLToPath(new URL('../../dist/anamnesis-daemon.mjs', import.meta.url));
const deadline = () => AbortSignal.timeout(10000);
function encode(value) { const b = Buffer.from(typeof value === 'string' ? value : JSON.stringify(value)); const f = Buffer.alloc(4+b.length); f.writeUInt32BE(b.length); b.copy(f,4); return f; }
function peer(path) {
  const socket = connect(path); const waiters = []; let buffer = Buffer.alloc(0);
  socket.on('error', e => { for (const w of waiters.splice(0)) w.reject(e); });
  socket.on('data', chunk => { buffer = Buffer.concat([buffer,chunk]); while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) { const n=buffer.readUInt32BE(); const value=JSON.parse(buffer.subarray(4,4+n).toString()); buffer=buffer.subarray(4+n); waiters.shift()?.resolve(value); } });
  socket.on('close', () => { for (const w of waiters.splice(0)) w.reject(Error('connection closed')); });
  let id=0;
  return { socket,
    frame(bytes, fragmented=false) { return new Promise((resolve,reject) => {
      const timer=setTimeout(()=>reject(Error('RPC deadline')),5000);
      waiters.push({resolve:x=>{clearTimeout(timer);resolve(x)},reject:e=>{clearTimeout(timer);reject(e)}});
      if(fragmented) for(const byte of bytes) socket.write(Buffer.from([byte])); else socket.write(bytes);
    }); },
    send(value) { return this.frame(encode(value)); },
    request(method,params={}) { return this.send({jsonrpc:'2.0',id:++id,method,params}); }
  };
}
async function fixture(run, configure = async () => ({})) {
  const root=await mkdtemp('/tmp/ana-process-'); const path=root+'/anamnesis.sock';
  let child, lines; const sockets=[];
  try {
    const env = { ...process.env };
    for (const name of Object.keys(env)) if (/^ANAMNESIS_(LLM_|EMBEDDING_|EXTRACTION_)/.test(name)) delete env[name];
    Object.assign(env, await configure(root));
    child=spawn(process.execPath,[entry],{env:{...env,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:'correct-token',ANAMNESIS_NEO4J_PASSWORD:'unused-offline',ANAMNESIS_NEO4J_URI:'bolt://127.0.0.1:1'},stdio:['ignore','pipe','pipe']});
    lines=createInterface({input:child.stdout});
    const appeared=(async()=>{
      for await (const line of lines) if (JSON.parse(line).event==='listening') return;
      throw Error('daemon stdout ended before listening');
    })();
    let output='';child.stderr.on('data',b=>{output+=b});
    const failed=new Promise((_,reject)=>{child.once('error',reject);child.once('exit',(c)=>reject(Error(`daemon exited ${c}: ${output}`)));});
    await Promise.race([appeared,failed,new Promise((_,reject)=>{const s=deadline();s.addEventListener('abort',()=>reject(Error('socket deadline')),{once:true})})]);
    lines.close();
    await run({root,path,child,client:()=>{const p=peer(path);sockets.push(p.socket);return p;}});
  } finally {
    lines?.close();for(const s of sockets)s.destroy();
    if(child && child.exitCode===null && child.signalCode===null){const exited=once(child,'exit',{signal:deadline()});child.kill('SIGKILL');await exited;}
    await rm(root,{recursive:true,force:true});
    await assert.rejects(stat(root),{code:'ENOENT'});
  }
}
const hello = { token:'correct-token',client:'node-test',commit_mode:'receipt',version:1 };
const params = { episode:{schema:'anamnesis.original-message/1',time:{value:'2026-09-09T00:00:00Z',precision:'second'},content:'durable Node request',origin:{source:'test',session:'s',actor:'a',record:'r'},mass:1,properties:{}},source_revision:'r1',expected_previous_revision_key:null };
const sha = value => createHash('sha256').update(JSON.stringify(value)).digest('hex');
// Independent canonical serialization: JSON.stringify's property list imposes
// sorted keys recursively, rather than sharing the daemon's digest helper.
function bodyDigest(params) {
  const envelope={digest_version:1,params};const keys=new Set();
  const collect=value=>{if(value && typeof value==='object'){for(const [key,child] of Object.entries(value)){if(!Array.isArray(value))keys.add(key);collect(child);}}};
  collect(envelope);
  return createHash('sha256').update(JSON.stringify(envelope,[...keys].sort())).digest('hex');
}
test('actual Node/socket reports unconfigured providers as disabled', {timeout:15000}, ()=>fixture(async ({client})=>{
  const p=client();
  const h=(await p.request('hello',hello)).result;
  const s=(await p.request('status')).result;
  for (const result of [h,s]) {
    assert.equal(result.capabilities.extraction,false);
    assert.equal(result.capabilities.embeddings,false);
  }
}));
for (const model of ['claude-haiku-4-5', 'gpt-5-5']) {
  test(`actual Node/socket advertises configured ${model} extraction without embeddings`, {timeout:15000}, ()=>fixture(async ({client})=>{
    const p=client();
    const h=(await p.request('hello',hello)).result;
    const s=(await p.request('status')).result;
    for (const result of [h,s]) {
      assert.equal(result.capabilities.extraction,true);
      assert.equal(result.capabilities.embeddings,false);
    }
  }, async root=>{
    await writeFile(root+'/provider.json',JSON.stringify({bearer:'provider-test-fixture'}),{mode:0o600});
    return {ANAMNESIS_LLM_BASE_URL:'http://127.0.0.1:1',ANAMNESIS_LLM_API_KEY_FILE:root+'/provider.json',ANAMNESIS_LLM_MODEL:model};
  }));
}
test('actual Node/socket advertises optional embedding configuration', {timeout:15000}, ()=>fixture(async ({client})=>{
  const p=client();
  const h=(await p.request('hello',hello)).result;
  const s=(await p.request('status')).result;
  for (const result of [h,s]) {
    assert.equal(result.capabilities.extraction,false);
    assert.equal(result.capabilities.embeddings,true);
  }
}, async ()=>({ANAMNESIS_EMBEDDING_BASE_URL:'http://127.0.0.1:1'})));
test('actual Node/socket rejects malformed JSON and unknown methods', {timeout:15000}, ()=>fixture(async ({client})=>{
  const p=client();assert.equal((await p.send('{')).error.data.code,'parse_error');
  assert.equal((await p.request('hello',hello)).result.principal,'installation');
  assert.equal((await p.request('invented.method',{})).error.data.code,'unsupported_method');
}));
test('actual Node/socket authenticates token and prevents pre-hello remember', {timeout:15000}, ()=>fixture(async ({client,root})=>{
  const p=client();assert.equal((await p.request('remember',params)).error.data.code,'unauthenticated');
  assert.equal((await p.request('hello',{...hello,token:'wrong'})).error.data.code,'authentication_failed');
  const valid=client();assert.equal((await valid.request('hello',hello)).result.principal,'installation');
  assert.equal((await stat(root)).mode&0o777,0o700);assert.equal((await stat(root+'/token')).mode&0o777,0o600);
}));
test('actual Node/socket durably accepts offline remember with real revision identity and bounded status', {timeout:15000}, ()=>fixture(async ({client,root})=>{
  const p=client();const h=(await p.request('hello',hello)).result;
  const result=(await p.request('remember',params)).result;
  assert.equal(result.state,'spooled');assert.equal(result.revision_key,sha([sha(['test','s','a','r']),'r1']));
  assert.equal(result.data_incarnation,h.data_incarnation);
  assert.equal(result.body_digest,bodyDigest(params));
  const duplicate=(await p.request('remember',params)).result;assert.deepEqual(duplicate,result);
  // Wire insertion order and an omitted default do not change the normalized
  // admission envelope; JSON-RPC request IDs are not part of delivery identity.
  const {properties,...episode}=params.episode;
  const normalizedDuplicate=(await p.request('remember',{expected_previous_revision_key:null,source_revision:'r1',episode})).result;
  assert.deepEqual(normalizedDuplicate,result);
  assert.equal((await p.request('remember',{...params,episode:{...params.episode,content:'different'}})).error.data.code,'revision_conflict');
  const s=(await p.request('status')).result;assert.equal(s.state,'degraded');assert.equal(s.storage,'unavailable');assert.equal(s.spool.pending,1);assert.ok(s.spool.bytes>0);
  const {revision_key,body_digest,data_incarnation}=result;
  assert.deepEqual((await p.request('ingest.status',{revision_key,body_digest,data_incarnation})).result,result);
  assert.equal((await p.request('ingest.status',{revision_key,body_digest:'0'.repeat(64),data_incarnation})).result.state,'unknown');
}));
test('G005 RED: real Node/UDS backup restore require auth and refuse without trusted adapter', {timeout:15000}, ()=>fixture(async ({client,root})=>{
  const p=client();
  const operation_id='01900000-0000-7000-8000-000000000001';
  // Both paths are unique, daemon-owned fixture paths. The runtime is pointed
  // at an unreachable, non-shared Bolt endpoint, so this test cannot touch a
  // production database or accidentally accept a manifest-only operation.
  const destination=root+'/owned-backup', archive=root+'/owned-archive';
  assert.equal((await p.request('backup',{operation_id,destination})).error.data.code,'unauthenticated');
  assert.equal((await p.request('hello',hello)).result.principal,'installation');
  assert.equal((await p.request('backup',{operation_id,destination})).error.data.code,'backup_adapter_unavailable');
  assert.equal((await p.request('restore',{operation_id,archive})).error.data.code,'restore_adapter_unavailable');
  assert.deepEqual((await p.request('backup.status',{operation_id})).result,{state:'unknown',operation_id,reason:'adapter_unavailable'});
  assert.deepEqual((await p.request('restore.status',{operation_id})).result,{state:'unknown',operation_id,reason:'adapter_unavailable'});
  await assert.rejects(stat(destination),{code:'ENOENT'});
  await assert.rejects(stat(archive),{code:'ENOENT'});
  await assert.rejects(stat(root+'/backup.state'),{code:'ENOENT'});
  await assert.rejects(stat(root+'/restore.state'),{code:'ENOENT'});
}));
test('actual Node/socket exposes spool quarantine and rejects new ingestion before object lookup', {timeout:15000}, ()=>fixture(async ({client,root})=>{
  const p=client();await p.request('hello',hello);
  const receipt=(await p.request('remember',params)).result;
  assert.equal(receipt.state,'spooled');
  // Corrupt opaque journal bytes, without interpreting or writing producer
  // marker/cursor formats. Quarantine is observed only through RPC status.
  const journal=root+'/spool/spool.journal';const bytes=await readFile(journal);
  bytes[bytes.length-1]^=1;await writeFile(journal,bytes);
  const status=(await p.request('status')).result;
  assert.equal(status.spool.quarantined,1);assert.equal(status.state,'degraded');
  const bindings=await readdir(root+'/deliveries');
  const next={...params,source_revision:'new-after-quarantine',payload_hash:'0'.repeat(64)};
  assert.equal((await p.request('remember',next)).error.data.code,'spool_corrupt');
  assert.deepEqual(await readdir(root+'/deliveries'),bindings);
  const {revision_key,body_digest,data_incarnation}=receipt;
  assert.equal((await p.request('ingest.status',{revision_key,body_digest,data_incarnation})).result.state,'quarantined');
}));
test('actual Node/socket handles split u32-be frames and rejects oversize/invalid UTF-8 before decoding', {timeout:15000}, ()=>fixture(async ({client})=>{
  const p=client();
  assert.equal((await p.frame(encode({jsonrpc:'2.0',id:1,method:'hello',params:hello}),true)).result.version,1);
  const utf8=Buffer.from([0,0,0,1,0xff]);
  assert.equal((await p.frame(utf8)).error.data.code,'parse_error');
  const oversized=Buffer.alloc(4);oversized.writeUInt32BE(1024*1024+1);
  const closed=once(p.socket,'close',{signal:deadline()});
  assert.equal((await p.frame(oversized)).error.data.code,'invalid_request');await closed;
}));
test('actual Node/socket rejects version mismatch and repeated hello', {timeout:15000}, ()=>fixture(async ({client})=>{
  const p=client();assert.equal((await p.request('hello',{...hello,version:2})).error.data.code,'unsupported_version');
  assert.equal((await p.request('hello',hello)).result.version,1);
  assert.equal((await p.request('hello',hello)).error.data.code,'already_authenticated');
}));
test('actual Node/socket enforces upload ownership, sequence, size, digest and reservations', {timeout:15000}, ()=>fixture(async ({client,root})=>{
  const p=client();await p.request('hello',hello);
  const other=client();await other.request('hello',hello);
  const bytes=Buffer.from('payload');const hash=createHash('sha256').update(bytes).digest('hex');
  const begin={sha256:hash,size:bytes.length,media_type:'text/plain'};
  const a=(await p.request('object.begin',begin)).result;
  const b=(await p.request('object.begin',begin)).result;
  assert.equal((await p.request('object.begin',begin)).error.data.code,'resource_exhausted');
  assert.equal((await other.request('object.chunk',{upload_id:a.upload_id,seq:0,bytes_b64:bytes.toString('base64')})).error.data.code,'upload_not_found');
  assert.equal((await p.request('object.chunk',{upload_id:a.upload_id,seq:1,bytes_b64:bytes.toString('base64')})).error.data.code,'upload_sequence_mismatch');
  assert.equal((await p.request('object.chunk',{upload_id:a.upload_id,seq:0,bytes_b64:Buffer.alloc(bytes.length+1).toString('base64')})).error.data.code,'object_size_mismatch');
  await p.request('object.chunk',{upload_id:a.upload_id,seq:0,bytes_b64:bytes.toString('base64')});
  assert.equal((await p.request('object.commit',{upload_id:a.upload_id})).result.hash,hash);
  assert.equal((await stat(`${root}/objects/${hash.slice(0,2)}/${hash}`)).mode&0o777,0o600);
  await p.request('object.chunk',{upload_id:b.upload_id,seq:0,bytes_b64:Buffer.alloc(bytes.length).toString('base64')});
  assert.equal((await p.request('object.commit',{upload_id:b.upload_id})).error.data.code,'object_hash_mismatch');
}));
test('actual Node/socket shutdown returns stopping and removes owner/socket', {timeout:15000}, ()=>fixture(async ({client,child,root})=>{
  const p=client();await p.request('hello',hello);
  const exited=once(child,'exit',{signal:deadline()});
  assert.equal((await p.request('shutdown')).result.state,'stopping');
  assert.equal((await exited)[0],0);
  await assert.rejects(stat(root+'/anamnesis.sock'),{code:'ENOENT'});
  await assert.rejects(stat(root+'/owner'),{code:'ENOENT'});
}));
