import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn, execFile } from 'node:child_process';
import { promisify } from 'node:util';
import { mkdtemp, mkdir, writeFile, readFile, rm, stat } from 'node:fs/promises';
import { join, resolve } from 'node:path';
import { createHash } from 'node:crypto';
import { fixture, canonical, hash } from './archive-manifest.fixture.mjs';

const entry = resolve('dist/anamnesis-ops.mjs');
const OWNER_LABEL = 'anamnesis.qa.owner';
const execute = promisify(execFile);
function environment(root) {
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_NEO4J_URI: 'bolt://127.0.0.1:1', ANAMNESIS_NEO4J_PASSWORD: 'g3-unused-password' };
  for (const key of Object.keys(env)) if (/^ANAMNESIS_(LLM_|EXTRACTION_|EMBEDDING_|NEO4J_CONTAINER|QA_OWNER)/.test(key)) delete env[key];
  return env;
}
async function cli(args, env) {
  const child = spawn(process.execPath, [entry, ...args], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  let stdout = '', stderr = '';
  child.stdout.on('data', b => stdout += b); child.stderr.on('data', b => stderr += b);
  const code = await new Promise((resolve, reject) => { child.once('error', reject); child.once('close', resolve); });
  return { code, stdout, stderr };
}
async function rootFor(t) {
  const root = await mkdtemp('/tmp/ana-g3-');
  t.after(() => rm(root, { recursive: true, force: true }));
  return root;
}

test('up without extraction configuration fails closed', { timeout: 15000 }, async t => {
  const root = await rootFor(t), result = await cli(['up'], environment(root));
  assert.equal(result.code, 2, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), { error: 'extraction_provider_required' });
  await assert.rejects(stat(join(root, 'anamnesis.sock')), { code: 'ENOENT' });
});

test('embed without a provider is a successful no-op', { timeout: 15000 }, async t => {
  const result = await cli(['embed'], environment(await rootFor(t)));
  assert.equal(result.code, 0, result.stderr);
  assert.deepEqual(JSON.parse(result.stdout), { embeddings: 'disabled', drained: 0 });
});

test('backup refuses a live unfenced owner', { timeout: 15000 }, async t => {
  const root = await rootFor(t);
  await mkdir(join(root, 'owner'), { mode: 0o700 });
  await writeFile(join(root, 'owner/owner.json'), JSON.stringify({ pid: process.pid, nonce: 'live-test-owner' }), { mode: 0o600 });
  const result = await cli(['backup', root + '-archive'], environment(root));
  assert.equal(result.code, 1);
  assert.equal(JSON.parse(result.stdout).error, 'daemon_live');
  await assert.rejects(stat(root + '-archive'), { code: 'ENOENT' });
});

test('restore rejects a tampered object with existing member_mismatch', { timeout: 15000 }, async t => {
  const f = await fixture(t);
  // Keep the archive compatible with the concrete pinned adapter, while preserving
  // valid canonical metadata and manifest hashes. The payload alone is corrupted.
  const image = 'sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d';
  f.manifest.compatibility.neo4j_version = '5.26.30';
  f.manifest.compatibility.neo4j_image_digest = image;
  const dump = f.manifest.members.find(m => m.role === 'database_dump');
  await f.replace('database/neo4j.dump.metadata.json', canonical({ format: 'anamnesis.archive-dump/1', database: 'neo4j', dump_path: dump.path, bytes: dump.bytes, sha256: dump.sha256, neo4j_version: '5.26.30', neo4j_image_digest: image }));
  const path = join(f.root, f.dataPath), bytes = await readFile(path); bytes[0] ^= 1; await writeFile(path, bytes);
  const root = await rootFor(t), result = await cli(['restore', f.root], environment(root));
  assert.equal(result.code, 1);
  assert.equal(JSON.parse(result.stderr).code, 'member_mismatch', result.stderr);
});

// Full real-container acceptance is deliberately unconditional: inability to run
// Docker or obtain the pinned image fails, rather than silently skipping coverage.
test('CLI backup and restore into an empty root then verify', { timeout: 240000 }, async t => {
  const owner = `g3-${process.pid}-${Date.now()}`, parent = await rootFor(t);
  const root = join(parent, 'source'), restored = join(parent, 'restored'), archive = join(parent, 'archive');
  const containers = [], ports = [];
  const evidence = resolve('.omo/evidence/runtime-complete/g3');
  await mkdir(evidence, { recursive: true });
  let transcript = '';
  const run = async (args, env, expected = 0) => {
    const result = await cli(args, env);
    transcript += `$ node dist/anamnesis-ops.mjs ${args.join(' ')}\n${result.stdout}${result.stderr}exit=${result.code}\n`;
    await writeFile(join(evidence, 'cli-transcript.log'), transcript);
    assert.equal(result.code, expected, result.stderr); return result;
  };
  t.after(async () => {
    for (const r of [root, restored]) { const result = await cli(['down'], environment(r)); transcript += `cleanup down: exit=${result.code}\n`; }
    let receipt = `# G3 Neo4j cleanup\nOwner: ${owner}\n`;
    for (const r of [root, restored]) {
      try { const state = JSON.parse(await readFile(join(r, 'authority.json'), 'utf8')); if (!containers.includes(state.container)) containers.push(state.container); }
      catch (e) { if (e.code !== 'ENOENT') throw e; }
    }
    const labelled = (await execute('docker', ['ps', '-aq', '--filter', `label=${OWNER_LABEL}=${owner}`])).stdout.trim().split('\n').filter(Boolean);
    containers.splice(0, containers.length, ...labelled);
    for (const id of containers) {
      const inspection = JSON.parse((await execute('docker', ['inspect', id])).stdout)[0];
      assert.equal(inspection.Config.Labels['anamnesis.qa.owner'], owner);
      const logs = await execute('docker', ['logs', '--tail', '40', id]).catch(error => ({ stdout: '', stderr: String(error) }));
      receipt += `\nContainer logs before removal:\n${logs.stdout || logs.stderr}\n`;
      const result = await execute('docker', ['rm', '-f', '-v', id]);
      receipt += `\nContainer: ${id}\ndocker rm -f -v: ${result.stdout.trim()}\n`;
    }
    const { createServer } = await import('node:net');
    for (const port of ports) {
      const deadline = Date.now() + 10000;
      while (true) {
        const server = createServer();
        try {
          await new Promise((resolve, reject) => { server.once('error', reject); server.listen(port, '127.0.0.1', resolve); });
          await new Promise((resolve, reject) => server.close(e => e ? reject(e) : resolve()));
          break;
        } catch (error) { server.close(); if (error.code !== 'EADDRINUSE' || Date.now() >= deadline) throw error; await new Promise(resolve => setImmediate(resolve)); }
      }
      receipt += `Port ${port}: successfully rebound after removal\n`;
    }
    await writeFile(join(evidence, 'neo4j-cleanup-receipt.md'), receipt);
  });
  const image = 'neo4j@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d';
  const id = (await execute('docker', ['run', '-d', '--label', `anamnesis.qa.owner=${owner}`, '-p', '127.0.0.1::7687', '-e', 'NEO4J_AUTH=neo4j/g3-isolated-password', '-e', 'NEO4J_server_memory_heap_initial__size=256m', '-e', 'NEO4J_server_memory_heap_max__size=512m', '-e', 'NEO4J_server_memory_pagecache_size=128m', image])).stdout.trim();
  containers.push(id);
  const port = Number((await execute('docker', ['port', id, '7687/tcp'])).stdout.trim().split(':').at(-1)); ports.push(port);
  const { default: neo4j } = await import('neo4j-driver');
  const driver = neo4j.driver(`bolt://127.0.0.1:${port}`, neo4j.auth.basic('neo4j', 'g3-isolated-password'), { connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
  try {
    const end = Date.now() + 90000;
    while (true) { try { await driver.verifyConnectivity(); break; } catch (e) { if (Date.now() >= end) throw e; } }
  } finally { await driver.close(); }
  const env = { ...environment(root), ANAMNESIS_NEO4J_URI: `bolt://127.0.0.1:${port}`, ANAMNESIS_NEO4J_PASSWORD: 'g3-isolated-password', ANAMNESIS_NEO4J_CONTAINER: id, ANAMNESIS_QA_OWNER: owner, ANAMNESIS_LLM_BASE_URL: 'http://127.0.0.1:1', ANAMNESIS_LLM_MODEL: 'gpt-fixture' };
  // This is a local inert fixture credential, never a token-hub bearer. Ingest
  // does not call extraction, and no fixture endpoint is presented as live LLM QA.
  const credential = join(parent, 'provider.json'); await writeFile(credential, JSON.stringify({ bearer: 'inert-g3-fixture' }), { mode: 0o600 }); env.ANAMNESIS_LLM_API_KEY_FILE = credential;
  await run(['up'], env);
  const lines = [0, 1, 2].map(n => ({ episode: { schema: 'anamnesis.original-message/1', time: { value: '2026-09-19T00:00:00Z', precision: 'second' }, content: `G3 archived episode ${n}`, origin: { source: 'g3', session: 'isolated', actor: 'qa', record: `${n}` }, mass: 1, properties: {} }, source_revision: 'r1', expected_previous_revision_key: null }));
  const input = join(parent, 'episodes.jsonl'); await writeFile(input, lines.map(JSON.stringify).join('\n') + '\n', { mode: 0o600 });
  await run(['ingest', input, join(parent, 'checkpoint.json')], env);
  await run(['backup', archive], env, 1); // A live daemon must never be dumped.
  await run(['down'], env);
  await run(['backup', archive], env);
  const target = { ...env, ANAMNESIS_RUNTIME_ROOT: restored };
  delete target.ANAMNESIS_NEO4J_URI; delete target.ANAMNESIS_NEO4J_CONTAINER;
  await mkdir(restored, { mode: 0o700 });
  await run(['restore', archive], target);
  const state = JSON.parse(await readFile(join(restored, 'authority.json'), 'utf8'));
  ports.push(Number(new URL(state.uri).port));
  await run(['up'], target);
  await run(['verify'], target);
  await run(['down'], target);
  const manifest = JSON.parse(await readFile(join(archive, 'manifest.json'), 'utf8'));
  assert.equal(manifest.authority.members.length, 3);
  assert.equal(createHash('sha256').update(await readFile(join(archive, 'config.jsonc'))).digest('hex'), manifest.configuration.config_sha256);
});
