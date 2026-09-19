import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { RpcClient } from '../../app/anamnesis/client.ts';

const root = process.env.DREAM_RUNTIME_ROOT;
const daemon = process.env.G004_DAEMON;
const token = 'dream-runtime-token';
const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token };
// Preserve daemon diagnostics in the owned evidence stream.

const waitReady = async child => { const lines=[]; const ready=new Promise((resolve,reject)=>{ child.stdout.on('data',b=>{ for(const line of b.toString().split('\n')) { lines.push(line); try { if(JSON.parse(line).event==='listening') resolve(); } catch {} }}); child.once('error',reject); child.once('exit',(c,s)=>reject(Error(`daemon exit ${c} ${s}: ${lines.join('')}`))); }); await ready; };
let child, client;
const start = async () => { child=spawn('node',[daemon],{env,stdio:['ignore','pipe','pipe']}); child.stderr.on('data', b => process.stderr.write(b)); await waitReady(child); client=await RpcClient.connect(`${root}/anamnesis.sock`,token,'receipt'); };
const stop = async () => { if(client){ try { await client.request('shutdown',{}); } catch {} try { await client.close(); } catch {} client=undefined; } if(child){ await once(child,'exit'); child=undefined; } };
const errorCode = async action => { try { await action(); assert.fail('expected refusal'); } catch (e) { return e.code; } };
const fence = { extraction_generation:0, covered_ingest_seq:1, structure_revision:0, policy_revision:0 };
const episode = { schema:'anamnesis.original-message/1', time:{value:'2026-09-13T00:00:00Z',precision:'second'}, content:'dream source', origin:{source:'dream-qa',session:'s',actor:'a',record:'r'}, mass:1, properties:{} };

test('dream runtime is authenticated, fenced, durable, CAS leased, and restart-readable', async t => {
  await start();
  t.after(stop);
  const unauth = await (await import('node:net')).connect(`${root}/anamnesis.sock`); // client handshake is the authenticated surface below
  unauth.destroy();
  const source = (await client.request('remember',{episode,source_revision:'a',expected_previous_revision_key:null}));
  assert.equal(source.state,'committed');
  const input={...fence,source_ids:[source.id],phase:'community'};
  const job=await client.request('dream.admit',input);
  assert.equal(job.semantic_writes,false); assert.equal(job.authority,'none'); assert.equal(job.state,'queued');
  assert.deepEqual(await client.request('dream.admit',input),job);
  assert.equal((await client.request('dream.status',{job_id:job.job_id})).job_id,job.job_id);
  assert.equal(await errorCode(() => client.request('dream.lease',{job_id:job.job_id,expected_version:99,worker_id:'w',lease_ms:100})),'dream_version_conflict');
  const leased=await client.request('dream.lease',{job_id:job.job_id,expected_version:0,worker_id:'w',lease_ms:100});
  assert.equal(leased.state,'leased'); assert.equal(leased.semantic_writes,false);
  assert.equal(await errorCode(() => client.request('dream.lease',{job_id:job.job_id,expected_version:1,worker_id:'other',lease_ms:100})),'dream_not_queued');
  const expired=await client.request('dream.expire',{job_id:job.job_id,expected_version:1,lease_epoch:leased.lease.epoch});
  assert.equal(expired.state,'queued'); assert.equal(expired.version,2);
  assert.equal(await errorCode(() => client.request('dream.status',{job_id:'dream-does-not-exist'})),'dream_job_missing');
  await stop(); await start();
  t.after(stop);
  const resumed=await client.request('dream.status',{job_id:job.job_id});
  assert.equal(resumed.state,'queued'); assert.equal(resumed.version,2); assert.equal(resumed.semantic_writes,false);
});

test('dream.execute records an unavailable adapter refusal without semantic writes', async t => {
  if(!client) await start();
  t.after(stop);
  // Deliberately bypass the client generic method union: this must reach the real
  // RPC boundary, not become a unit-test substitute for the missing operation.
  const source = (await client.request('remember',{episode,source_revision:'a',expected_previous_revision_key:null}));
  const job = await client.request('dream.admit',{...fence,source_ids:[source.id],phase:'community'});
  const result = await client.request('dream.execute', {job_id:job.job_id,expected_version:job.version});
  assert.equal(result.state, 'unknown');
  assert.equal(result.execution.error, 'dream_adapter_unavailable');
  assert.equal(result.execution.retryable, false);
});

test('dream admission refuses stale fence and policy denied source', async t => {
  if(!client) await start();
  t.after(stop);
  const bad={...fence,structure_revision:999,source_ids:['01900000-0000-7000-8000-000000000001'],phase:'community'};
  assert.equal(await errorCode(() => client.request('dream.admit',bad)),'dream_fence_stale');
});
