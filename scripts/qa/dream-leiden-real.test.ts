import {expect,test} from 'bun:test';
import {mkdir,mkdtemp,writeFile} from 'node:fs/promises';
import {join,resolve} from 'node:path';
import {startProcess} from './runtime-scenarios.ts';

test('real Node UDS Dream Leiden', async()=>{
 expect(process.env.ANAMNESIS_TEST_NEO4J_PASSWORD).toBeTruthy();
 const evidence=resolve('.omo/evidence/dream-leiden-real');await mkdir(evidence,{recursive:true});
 const root=await mkdtemp(join(evidence,'node-'));
 const runtime=await mkdtemp('/tmp/dream-real-');
 const daemon=join(root,'daemon.mjs'),fixture=join(root,'fixture.mjs');
 for(const [source,out] of [['app/anamnesis/main.ts',daemon],['scripts/qa/dream-leiden-real.fixture.mjs',fixture]]){
  const build=await startProcess(process.execPath,['build',source!,'--target=node','--outfile',out!],{deadlineMs:120000}).done;
  await writeFile(`${out}.build.json`,JSON.stringify(build));expect(build.code).toBe(0);
 }
 const result=await startProcess('node',['--test',fixture],{deadlineMs:600000,env:{...process.env,G004_DAEMON:daemon,DREAM_RUNTIME_ROOT:runtime,ANAMNESIS_NEO4J_URI:process.env.ANAMNESIS_TEST_NEO4J_URI,ANAMNESIS_NEO4J_PASSWORD:process.env.ANAMNESIS_TEST_NEO4J_PASSWORD,ANAMNESIS_DREAM_GDS_ASSETS:join(evidence,'assets'),ANAMNESIS_DREAM_GDS_WORK:join(root,'gds')},onOutput:text=>process.stdout.write(text)}).done;
 await writeFile(join(root,'result.json'),JSON.stringify(result));expect(result.timedOut).toBe(false);expect(result.code).toBe(0);
},660000);
