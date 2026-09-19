import test from 'node:test';
import assert from 'node:assert/strict';
import {spawn} from 'node:child_process';
import {once} from 'node:events';
import {createInterface} from 'node:readline';
import {RpcClient} from '../../app/anamnesis/client.ts';
import neo4j from 'neo4j-driver';

test('authenticated dream.execute produces a durable real Leiden result', {timeout:300000}, async () => {
 const root=process.env.DREAM_RUNTIME_ROOT, token='dream-real-owned-token';
 const child=spawn('node',[process.env.G004_DAEMON],{env:{...process.env,ANAMNESIS_RUNTIME_ROOT:root,ANAMNESIS_RUNTIME_TOKEN:token},stdio:['ignore','pipe','pipe']});
 const exited=once(child,'exit');
 child.stderr.on('data',b=>process.stderr.write(b));
 const lines=createInterface({input:child.stdout});
 const ready=new Promise((resolve,reject)=>{const timer=setTimeout(()=>reject(Error('daemon readiness timeout')),30000);lines.on('line',line=>{try{if(JSON.parse(line).event==='listening'){clearTimeout(timer);resolve();}}catch(error){reject(error);}});child.once('error',reject);child.once('exit',()=>{clearTimeout(timer);reject(Error('daemon exited'));});});
 let client;
 try {
  await ready; client=await RpcClient.connect(`${root}/anamnesis.sock`,token,'receipt');
  const sources=[]; for(let i=0;i<3;i++) sources.push(await client.request('remember',{episode:{schema:'anamnesis.original-message/1',content:`dream graph source ${i}`,time:{value:'2026-09-13T00:00:00Z',precision:'second'},origin:{source:'dream-real',session:'s',actor:'a',record:`r${i}`},mass:1,properties:{}},source_revision:'v1',expected_previous_revision_key:null}));
  const driver=neo4j.driver(process.env.ANAMNESIS_NEO4J_URI,neo4j.auth.basic('neo4j',process.env.ANAMNESIS_NEO4J_PASSWORD));
  try { await driver.executeQuery(`MATCH (a:Element:Episode {id:$a}),(b:Element:Episode {id:$b}) CREATE (a)-[:RELATES_TO {weight:2.0}]->(b)`,{a:sources[0].id,b:sources[1].id}); await driver.executeQuery(`MATCH (a:Element:Episode {id:$a}),(b:Element:Episode {id:$b}) CREATE (a)-[:RELATES_TO {weight:1.0}]->(b)`,{a:sources[1].id,b:sources[2].id}); } finally {await driver.close();}
  const job=await client.request('dream.admit',{phase:'community',extraction_generation:0,covered_ingest_seq:sources[2].ingest_seq,structure_revision:0,policy_revision:0,source_ids:sources.map(s=>s.id)});
  const result=await client.request('dream.execute',{job_id:job.job_id,expected_version:job.version});
  console.log(JSON.stringify({checkpoint:'positive-execution',result}));
  assert.equal(result.state,'succeeded',JSON.stringify(result.execution));
 } finally {if(client){await client.request('shutdown',{});await client.close();}else child.kill();await exited;lines.close();}
});
