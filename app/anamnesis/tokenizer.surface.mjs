// Real built Node/UDS/owned Neo4j. Synthetic encoder, not model-vocabulary evidence.
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { once } from 'node:events';
import { createInterface } from 'node:readline';
import { mkdtemp, readFile, rm, writeFile, stat } from 'node:fs/promises';
import { randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { Socket } from 'node:net';
import neo4j from 'neo4j-driver';
import { RpcClient } from '../../dist/anamnesis-client.mjs';
import fixture from './tokenizer.fixture.cjs';
import { install, sha } from './tokenizer-install.fixture.mjs';

const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
assert.ok(uri && password, 'owned graph credentials required');
const red = process.env.TOKENIZER_PHASE === 'red';
const evidence = resolve(process.env.TOKENIZER_EVIDENCE ?? '.omo/evidence/g003-tokenizer-runtime');
const collisions = process.env.TOKENIZER_COLLISIONS === '1';
const root = await mkdtemp('/tmp/g003-token-'), token = randomUUID();
const code = await readFile('app/anamnesis/tokenizer.fixture.cjs');
const installedCode = collisions ? Buffer.concat([Buffer.from("const text = '', encode = null, assets = null, module = globalThis.module;\n"), code]) : code;
const config = await install(root, installedCode);
const driver = neo4j.driver(uri, neo4j.auth.basic('neo4j', password), { disableLosslessIntegers: true });
const query = async (cypher, params = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
const encode = fixture.createEncoder({ vocabulary: await readFile(config.assets[0].path) });
const log = (checkpoint, details) => console.log(JSON.stringify({ checkpoint, ...details }));
let daemon, client;
async function start(configuration = JSON.stringify(config), rejected = false) {
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token,
    ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password };
  delete env.ANAMNESIS_EMBEDDING_CONFIG;
  if (configuration === null) delete env.ANAMNESIS_TOKENIZER_CONFIG; else env.ANAMNESIS_TOKENIZER_CONFIG = configuration;
  const child = spawn(process.execPath, [red ? '/tmp/g003-red-daemon.mjs' : 'dist/anamnesis-daemon.mjs'], { env, stdio: ['ignore', 'pipe', 'pipe'] });
  const done = once(child, 'exit', { signal: AbortSignal.timeout(45000) });
  let stderr = '', listening = false;
  child.stderr.on('data', bytes => { stderr += bytes; });
  const lines = createInterface({ input: child.stdout });
  const ready = new Promise((fulfill, reject) => {
    const timer = setTimeout(() => reject(Error('daemon startup deadline')), 30000);
    lines.on('line', line => { const event = JSON.parse(line); log('daemon', event); if (event.event === 'listening') { listening = true; clearTimeout(timer); fulfill(); } });
    child.once('error', error => { clearTimeout(timer); reject(error); });
    child.once('exit', code => { clearTimeout(timer); rejected ? fulfill() : reject(Error(`daemon exited ${code}: ${stderr}`)); });
  });
  daemon = { child, done, lines }; await ready;
  if (rejected) {
    const [code] = await done; assert.equal(code, 1); assert.equal(listening, false); assert.ok(stderr);
    await assert.rejects(stat(root + '/anamnesis.sock'), { code: 'ENOENT' });
    await assert.rejects(stat(root + '/owner'), { code: 'ENOENT' });
    lines.close(); daemon = undefined;
    log('startup-rejected', { configuration, error: stderr.trim() }); return;
  }
  client = await RpcClient.connect(root + '/anamnesis.sock', token);
}
async function stop() {
  if (client) { await client.request('shutdown', {}); await client.close(); client = undefined; }
  if (daemon) { assert.equal((await daemon.done)[0], 0); daemon.lines.close(); daemon = undefined; }
}
async function remember(record, content, session = 'small') {
  const value = await client.request('remember', { episode: { schema: 'anamnesis.original-message/1', content, mass: 0, properties: {},
    time: { value: '2026-09-01T00:00:00Z', precision: 'second' }, origin: { source: root, session, actor: 'fixture', record } },
    source_revision: 'v1', expected_previous_revision_key: null });
  assert.equal(value.state, 'committed'); return value.id;
}
const request = { query: '', session: { source: root, session: 'small' }, limit: 2 };
const budget = limit => ({ unit: 'tokens', limit, tokenizer_id: config.id });
async function rawRecall(params) {
  const socket = new Socket();
  const connected = once(socket, 'connect', { signal: AbortSignal.timeout(5000) }); socket.connect(root + '/anamnesis.sock'); await connected;
  const exchange = request => new Promise((fulfill, reject) => {
    let buffered = Buffer.alloc(0);
    const cleanup = () => { clearTimeout(timer); socket.off('data', data); socket.off('error', error); socket.off('close', closed); };
    const error = cause => { cleanup(); reject(cause); };
    const closed = () => error(Error('raw peer closed before response'));
    const data = bytes => {
      buffered = Buffer.concat([buffered, bytes]);
      if (buffered.length < 4) return;
      const size = buffered.readUInt32BE();
      if (size > 1048576) return error(Error('oversized wire response'));
      if (buffered.length < size + 4) return;
      cleanup(); fulfill({ response: JSON.parse(buffered.subarray(4, 4 + size)), bytes: size });
    };
    const timer = setTimeout(() => error(Error('raw response deadline')), 10000);
    socket.on('data', data); socket.once('error', error); socket.once('close', closed);
    const json = Buffer.from(JSON.stringify(request)), frame = Buffer.alloc(4 + json.length);
    frame.writeUInt32BE(json.length); json.copy(frame, 4); socket.write(frame);
  });
  try {
    const hello = await exchange({ jsonrpc: '2.0', id: 1, method: 'hello', params: { token, client: 'tokenizer-raw', version: 1, commit_mode: 'receipt' } });
    assert.ok(hello.response.result);
    return await exchange({ jsonrpc: '2.0', id: 2, method: 'recall', params });
  } finally { socket.destroy(); }
}
async function receipt(result) {
  const row = (await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body', { id: result.recall_id }))[0];
  assert.ok(row); const stored = JSON.parse(row.body);
  assert.deepEqual(stored.serving.response, result);
  assert.deepEqual(stored.primary_ids, result.results.map(item => item.id));
  assert.equal(stored.serving.context_digest, sha(result.context_text));
  assert.equal(stored.serving.response.used_budget, encode(result.context_text));
  return { recall_id: result.recall_id, budget: stored.serving.response.budget, used_budget: result.used_budget, context_digest: stored.serving.context_digest };
}
try {
  await start();
  if (red) {
    await assert.rejects(client.request('recall', { ...request, budget: budget(100000) }), { code: 'invalid_budget' });
    log('RED-real-baseline-daemon', { configured: config, observed: 'invalid_budget', bundle_sha256: sha(await readFile('/tmp/g003-red-daemon.mjs')),
      limitation: 'baseline reconstructed by restoring runtime wiring after an initial implementation attempt; not a pre-edit capture' });
  } else {
    const ids = [await remember('a', 'needle A\né🙂 "quoted" \\path'), await remember('b', 'needle B\té🙂')];
    const all = await client.request('recall', { ...request, budget: budget(100000) });
    assert.deepEqual(new Set(all.results.map(item => item.id)), new Set(ids));
    assert.deepEqual(all.context_text.split('\n').map(line => JSON.parse(line)), all.results);
    assert.ok(all.context_text.length > 0 && all.used_budget > 0);
    assert.equal(all.used_budget, encode(all.context_text));
    assert.notEqual(all.used_budget, Buffer.byteLength(all.context_text));
    assert.notEqual(all.used_budget, [...all.context_text].length);
    const sum = all.context_text.split('\n').reduce((sum, text) => sum + encode(text), 0);
    assert.equal(encode(all.context_text), sum - 1, 'three-byte record boundary merges to one token');
    const exact = await client.request('recall', { ...request, budget: budget(all.used_budget) });
    assert.deepEqual(exact.results, all.results); assert.equal(exact.context_text, all.context_text);
    const below = await client.request('recall', { ...request, budget: budget(all.used_budget - 1) });
    assert.equal(below.results.length, 1);
    const zero = await client.request('recall', { ...request, budget: budget(0) });
    const limitZero = await client.request('recall', { ...request, limit: 0, budget: budget(100000) });
    for (const result of [zero, limitZero]) assert.deepEqual([result.results, result.companions, result.context_text, result.used_budget], [[], [], '', 0]);
    log('whole-context-exact-fit-one-below-zero', { sum_per_record: sum, exact: await receipt(exact), below: await receipt(below), zero: await receipt(zero), limit_zero: await receipt(limitZero) });
    await writeFile(evidence + '/green-receipt.json', JSON.stringify({ response: exact, stored: (await query('MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body', { id: exact.recall_id }))[0] }, null, 2));
    for (const unit of ['utf8_bytes', 'unicode_scalars']) {
      const n = unit === 'utf8_bytes' ? Buffer.byteLength(all.context_text) : [...all.context_text].length;
      const result = await client.request('recall', { ...request, budget: { unit, limit: n } });
      assert.deepEqual(result.results, all.results); assert.equal(result.used_budget, n);
      assert.equal((await client.request('recall', { ...request, budget: { unit, limit: n - 1 } })).results.length, 1);
      assert.equal((await client.request('recall', { ...request, budget: { unit, limit: 0 } })).used_budget, 0);
    }
    for (const tokenizer_id of ['unknown@sha256:' + 'f'.repeat(64), 'unpinned', undefined]) {
      const wire = await rawRecall({ ...request, budget: { unit: 'tokens', limit: 1, tokenizer_id } });
      assert.equal(wire.response.error.data.code, tokenizer_id?.startsWith('unknown@') ? 'invalid_budget' : 'invalid_params');
      log('invalid-tokenizer-over-UDS', { tokenizer_id: tokenizer_id ?? null, error: wire.response.error.data.code });
    }
    for (let i = 0; i < 18; i++) await remember('large-' + i, 'x'.repeat(63000), 'large');
    const boundedWire = await rawRecall({ query: '', session: { source: root, session: 'large' }, limit: 64, budget: budget(Number.MAX_SAFE_INTEGER) });
    assert.ok(boundedWire.response.result); const bounded = boundedWire.response.result;
    assert.ok(bounded.results.length > 0 && bounded.results.length < 18); assert.ok(bounded.diagnostics.skipped_bundles > 0);
    assert.ok(Buffer.byteLength(JSON.stringify(bounded)) < 1048576); assert.equal(bounded.used_budget, encode(bounded.context_text));
    assert.ok(bounded.results.every(item => item.content.length === 63000));
    log('1MiB-response-bound', { results: bounded.results.length, skipped: bounded.diagnostics.skipped_bundles, context_bytes: Buffer.byteLength(bounded.context_text), response_bytes: Buffer.byteLength(JSON.stringify(bounded)), actual_wire_frame_bytes: boundedWire.bytes, receipt: await receipt(bounded) });
    await stop(); await start();
    assert.deepEqual((await receipt(exact)).budget, budget(all.used_budget));
    log('receipt-survives-restart', await receipt(exact)); await stop();
    await start(null);
    await assert.rejects(client.request('recall', { ...request, budget: budget(1) }), { code: 'invalid_budget' }); await stop();
    for (const value of ['{', '{}', JSON.stringify({ ...config, id: 'unpinned' }), JSON.stringify({ ...config, extra: true }),
      JSON.stringify({ ...config, assets: [] }), JSON.stringify({ ...config, path: root + '/missing' }),
      JSON.stringify({ ...config, assets: [{ ...config.assets[0], path: root + '/missing-asset' }] }),
      JSON.stringify({ ...config, assets: [...config.assets, ...config.assets] }), JSON.stringify({ ...config, id: 'wrong@sha256:' + '0'.repeat(64) })]) await start(value, true);
    const original = await readFile(config.path), asset = await readFile(config.assets[0].path);
    await writeFile(config.path, Buffer.concat([original, Buffer.from('\n// tampered executable')])); await start(JSON.stringify(config), true); await writeFile(config.path, original);
    await writeFile(config.assets[0].path, Buffer.concat([asset, Buffer.from(' ')])); await start(JSON.stringify(config), true); await writeFile(config.assets[0].path, asset);
    for (const output of ['-1', '0.5', 'NaN', 'Infinity', 'Number.MAX_SAFE_INTEGER + 1']) {
      const invalid = await install(root, Buffer.from(`module.exports.createEncoder = () => text => text === '' ? 0 : ${output};`));
      await start(JSON.stringify(invalid));
      const before = (await query('MATCH (r:RecallReceipt) RETURN count(r) AS n'))[0].n;
      await assert.rejects(client.request('recall', { ...request, budget: { ...budget(100000), tokenizer_id: invalid.id } }), { code: 'invalid_budget' });
      assert.equal((await query('MATCH (r:RecallReceipt) RETURN count(r) AS n'))[0].n, before); await stop();
      log('invalid-count-rejected-without-receipt', { output });
    }
    log('GREEN-real-daemon', { collisions, tokenizer_id: config.id, code_sha256: sha(installedCode),
      asset_sha256: sha(asset), bundle_sha256: sha(await readFile('dist/anamnesis-daemon.mjs')) });
  }
  await stop();
} finally {
  await client?.close();
  if (daemon) { if (daemon.child.exitCode === null && daemon.child.signalCode === null) daemon.child.kill('SIGKILL'); await daemon.done; daemon.lines.close(); }
  await driver.close(); await rm(root, { recursive: true, force: true });
  await assert.rejects(stat(root), { code: 'ENOENT' }); log('cleanup', { root_removed: root });
}
