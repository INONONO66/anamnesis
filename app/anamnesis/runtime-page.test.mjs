import test from 'node:test';
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { mkdtemp, rm } from 'node:fs/promises';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';

// Build runtime.ts with --target=node to this private bundle before running.
const { Runtime } = await import(pathToFileURL(resolve(process.env.RUNTIME_MODULE ?? '.omo/evidence/runtime-page-integration/runtime-module.mjs')));
const canonical = value => {
  if (!value || typeof value !== 'object') return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`;
  return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`;
};
const hash = value => createHash('sha256').update(canonical(value)).digest('hex');
const key = params => { const o=params.episode.origin; return hash([hash([o.source,o.session,o.actor,o.record]),params.source_revision]); };
const input = (record, predecessor=null, revision='v1') => ({ episode:{schema:'anamnesis.original-message/1',time:{value:'2026-09-09T00:00:00Z',precision:'second'},content:record,origin:{source:'page-test',session:'s',actor:'a',record},mass:1,properties:{}},source_revision:revision,expected_previous_revision_key:predecessor });
const identity = receipt => ({revision_key:receipt.revision_key,body_digest:receipt.body_digest,data_incarnation:receipt.data_incarnation});

async function fixture(run) {
  const root=await mkdtemp('/tmp/ana-page-');
  const saved={password:process.env.ANAMNESIS_NEO4J_PASSWORD,uri:process.env.ANAMNESIS_NEO4J_URI};
  process.env.ANAMNESIS_NEO4J_PASSWORD='fixture-unused';process.env.ANAMNESIS_NEO4J_URI='bolt://127.0.0.1:1';
  const runtime=new Runtime({root,incarnation:randomUUID(),epoch:randomUUID(),assertOwned:async()=>{}});
  for(const [name,value] of [['ANAMNESIS_NEO4J_PASSWORD',saved.password],['ANAMNESIS_NEO4J_URI',saved.uri]]) {
    if(value===undefined)delete process.env[name];else process.env[name]=value;
  }
  // Only DB I/O is simulated. Runtime validation, committed(), write(), drain(),
  // public status/remember and the real fsynced DurableSpool all execute intact.
  const rows=new Map();const heads=new Map();const commits=[];let online=false;
  function publish(params) {
    const e=params.episode;const id=randomUUID();
    const origin=hash(e.origin);
    if ((heads.get(origin) ?? null)!==params.expected_previous_revision_key) throw Object.assign(Error('origin head CAS mismatch'),{code:'stale_revision'});
    heads.set(origin,key(params));
    rows.set(key(params),{id,schema:e.schema,time_value:e.time.value,time_precision:e.time.precision,content:e.content,mass:e.mass,properties:JSON.stringify(e.properties),origin_source:e.origin.source,origin_session:e.origin.session,origin_actor:e.origin.actor,origin_record:e.origin.record,source_revision:params.source_revision,previous_revision_key:params.expected_previous_revision_key,ingest_seq:rows.size+1,digest_format:'rfc8785-v1',digest:hash({schema:e.schema,content:e.content,properties:e.properties,time:e.time,payload_hash:null,previous_revision_key:params.expected_previous_revision_key})});
    return id;
  }
  runtime.read=async(query,params={})=>{
    if(!online)throw Object.assign(Error('offline fixture'),{code:'ServiceUnavailable'});
    if(query==='RETURN 1 AS connected')return [{connected:1}];
    if(query.includes('writer_epoch'))return [{epoch:1}];
    if(query.includes('properties(e) AS e'))return rows.has(params.key)?[{e:rows.get(params.key)}]:[];
    if(query.includes('count(e) AS found'))return [{found:rows.has(params.key)?1:0}];
    throw Error(`unexpected DB query: ${query}`);
  };
  runtime.engine.init=async()=>{};runtime.engine.claimWriterEpoch=async()=>1;
  runtime.engine.status=async()=>({pendingOutbox:rows.size});
  runtime.engine.remember=async({source_revision,expected_previous_revision_key,...episode})=>{
    const params={episode,source_revision,expected_previous_revision_key};
    assert.ok(!rows.has(key(params)),'existing rows must be verified without another engine write');
    if(expected_previous_revision_key)assert.ok(rows.has(expected_previous_revision_key),'dependency must commit before child');
    const id=publish(params);commits.push(key(params));return {id,created:true};
  };
  try {
    await runtime.init();
    await run({runtime,rows,commits,publish,online:async()=>{
      online=true; await runtime.refresh();
      // Drive exactly the work requested by Runtime; this is serial dispatch,
      // not polling for a desired state or waiting for timing luck.
      let turns=0;
      while(await runtime.drainTurn()) assert.ok(++turns<20000,'finite drain must settle');
    },offline:()=>{online=false;},
      // Deterministic fail-fast guard: the real complete executes, but a repeated
      // no-progress scan fails by operation count, not a timer or a fake loop.
      guardCompletions(limit) {
        const complete=runtime.spool.complete.bind(runtime.spool);const calls=[];
        runtime.spool.complete=async sequence=>{calls.push(sequence);assert.ok(calls.length<=limit,`drain repeated completion without settling: ${calls.join(',')}`);await complete(sequence);};
        return calls;
      }});
  } finally {await runtime.close();await rm(root,{recursive:true,force:true});}
}

test('Runtime drain settles with blocked head and already-completed retained suffix', {timeout:10000},()=>fixture(async({runtime,publish,online,offline,guardCompletions})=>{
  const head=await runtime.remember(input('head','f'.repeat(64)));
  const suffixInput=input('suffix');const suffix=await runtime.remember(suffixInput);
  publish(suffixInput);await runtime.spool.complete(suffix.spool_seq);
  const calls=guardCompletions(4);await online();
  const status=await runtime.status(0,false);
  assert.equal(status.spool.pending,2);assert.equal(status.spool.blocked,1);
  assert.equal(calls.length,1,'idempotent suffix is completed only once per settled drain');
  assert.equal((await runtime.ingestStatus(identity(head))).reason,'missing_predecessor');
  assert.equal((await runtime.ingestStatus(identity(suffix))).state,'committed');
  offline();
  assert.equal((await runtime.ingestStatus(identity(suffix))).state,'spooled','local completion cannot become committed while DB is offline');
  assert.deepEqual(await runtime.remember(suffixInput),suffix,'retained suffix dedupe must preserve receipt');
  assert.equal((await runtime.ingestStatus({...identity(suffix),body_digest:'0'.repeat(64)})).state,'unknown');
}));

test('Runtime executes a later-page predecessor and revisits its earlier child behind a blocked head', {timeout:30000},()=>fixture(async({runtime,publish,online,commits,guardCompletions})=>{
  await runtime.remember(input('head','f'.repeat(64)));
  const parent=input('chain',null,'v1');const child=input('chain',key(parent),'v2');
  const receipt=await runtime.remember(child);
  for(let i=0;i<98;i++) {
    const params=input(`retained-${i}`);const item=await runtime.remember(params);
    publish(params);await runtime.spool.complete(item.spool_seq);
  }
  const parentReceipt=await runtime.remember(parent);assert.equal(parentReceipt.spool_seq,101);
  guardCompletions(500);await online();
  const status=await runtime.status(0,false);
  assert.deepEqual(commits,[key(parent),key(child)]);
  assert.equal(status.spool.pending,101);assert.equal(status.spool.blocked,1);
  assert.equal((await runtime.ingestStatus(identity(receipt))).state,'committed');
}));

test('Runtime classifies an acyclic missing-predecessor chain without completing blocked records', {timeout:10000},()=>fixture(async({runtime,online,guardCompletions})=>{
  const missing=input('missing-chain',null,'absent');
  const a=input('missing-chain',key(missing),'a');
  const b=input('missing-chain',key(a),'b');
  const c=input('missing-chain',key(b),'c');
  const receipts=[];
  for(const params of [c,b,a])receipts.push(await runtime.remember(params));
  const calls=guardCompletions(0);await online();
  for(const receipt of receipts)assert.equal((await runtime.ingestStatus(identity(receipt))).reason,'missing_predecessor');
  assert.deepEqual(calls,[]);assert.equal((await runtime.spool.status()).pending,3);
}));

test('Runtime distinguishes cycle members from an acyclic tail into the cycle', {timeout:10000},()=>fixture(async({runtime,online,guardCompletions})=>{
  const a=input('cycle',null,'a');const b=input('cycle',key(a),'b');a.expected_previous_revision_key=key(b);
  const tail=input('cycle',key(a),'tail');
  const receipts=[];for(const params of [tail,a,b])receipts.push(await runtime.remember(params));
  const calls=guardCompletions(0);await online();
  assert.equal((await runtime.ingestStatus(identity(receipts[0]))).reason,'missing_predecessor');
  for(const receipt of receipts.slice(1))assert.equal((await runtime.ingestStatus(identity(receipt))).reason,'dependency_cycle');
  assert.deepEqual(calls,[]);assert.equal((await runtime.spool.status()).pending,3);
}));

test('Runtime treats an invalid retained predecessor as missing, not a cycle', {timeout:10000},()=>fixture(async({runtime,online,guardCompletions})=>{
  const a=input('invalid',null,'a');const b=input('invalid',key(a),'b');const c=input('invalid',key(b),'c');
  const receipts=[];for(const params of [c,b])receipts.push(await runtime.remember(params));
  const o=a.episode.origin;const incarnation=runtime.installation.incarnation;
  await runtime.spool.append({origin:hash([o.source,o.session,o.actor,o.record]),revision:key(a),predecessor:null,incarnation,
    body:{digest_version:1,params:a,body_digest:'0'.repeat(64),incarnation,fs_epoch:runtime.installation.epoch}});
  const calls=guardCompletions(0);await online();
  for(const receipt of receipts)assert.equal((await runtime.ingestStatus(identity(receipt))).reason,'missing_predecessor');
  const status=await runtime.status(0,false);assert.equal(status.spool.quarantined,1);
  assert.deepEqual(calls,[]);assert.equal(status.spool.pending,3);
}));

test('Runtime propagates a stale same-origin terminus as missing, not a cycle', {timeout:10000},()=>fixture(async({runtime,publish,online,guardCompletions})=>{
  publish(input('stale',null,'current'));
  const a=input('stale',null,'a');const b=input('stale',key(a),'b');const c=input('stale',key(b),'c');
  const receipts=[];for(const params of [c,b,a])receipts.push(await runtime.remember(params));
  const calls=guardCompletions(0);await online();
  assert.equal((await runtime.ingestStatus(identity(receipts[2]))).reason,'stale_revision');
  for(const receipt of receipts.slice(0,2))assert.equal((await runtime.ingestStatus(identity(receipt))).reason,'missing_predecessor');
  assert.deepEqual(calls,[]);assert.equal((await runtime.spool.status()).pending,3);
}));

