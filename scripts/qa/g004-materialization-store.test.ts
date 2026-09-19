import { expect, test } from 'bun:test';
import { mkdtemp, writeFile } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { startProcess } from './runtime-scenarios.ts';

test('retained materialization persists valid identities through real Node and owned Neo4j', async () => {
  expect(process.env.ANAMNESIS_TEST_NEO4J_URI).toBeTruthy();
  expect(process.env.ANAMNESIS_TEST_NEO4J_PASSWORD).toBeTruthy();
  const parent = resolve(process.env.G004_MATERIALIZATION_EVIDENCE ?? '.omo/evidence/g004-materialization-store');
  const root = await mkdtemp(join(parent, 'node-'));
  const bundle = join(root, 'fixture.mjs');
  const daemon = join(root, 'daemon.mjs');
  const daemonBuild = await startProcess(process.execPath, ['build', 'app/anamnesis/main.ts', '--target=node', '--outfile', daemon], { deadlineMs: 120000 }).done;
  await writeFile(join(root, 'daemon-build.json'), JSON.stringify(daemonBuild, null, 2));
  expect(daemonBuild.code).toBe(0);
  process.env.G004_DAEMON = daemon;
  const build = await startProcess(process.execPath, ['build', 'scripts/qa/g004-materialization-store.fixture.mjs', '--target=node', '--outfile', bundle], { deadlineMs: 120000 }).done;
  await writeFile(join(root, 'build.json'), JSON.stringify(build, null, 2));
  expect(build.code).toBe(0);
  const result = await startProcess('node', ['--test', bundle], { deadlineMs: 240000, env: { ...process.env, G004_DAEMON: daemon }, onOutput: text => process.stdout.write(text) }).done;
  await writeFile(join(root, 'result.json'), JSON.stringify(result, null, 2));
  expect(result.timedOut).toBe(false);
  expect(result.code).toBe(0);
}, 380000);
