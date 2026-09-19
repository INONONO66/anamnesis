import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdtemp, readFile, writeFile, rm, truncate } from 'node:fs/promises';
import { pathToFileURL } from 'node:url';
import { resolve } from 'node:path';
const { loadTokenizers } = await import(process.env.TOKENIZER_MODULE
  ? pathToFileURL(resolve(process.env.TOKENIZER_MODULE)).href
  : new URL('../../dist/anamnesis-tokenizer.mjs', import.meta.url).href);
import { install, sha } from './tokenizer-install.fixture.mjs';
import fixture from './tokenizer.fixture.cjs';

async function installed(run) {
  const root = await mkdtemp('/tmp/g003-loader-');
  try { const config = await install(root); await run(config, root); }
  finally { await rm(root, { recursive: true, force: true }); }
}
test('installed executable and assets, whole-context merges, Unicode and exact 1MiB input bound', { timeout: 15000 }, () => installed(async config => {
  const encode = loadTokenizers(JSON.stringify(config)).get(config.id);
  const expected = fixture.createEncoder({ vocabulary: await readFile(config.assets[0].path) });
  for (const text of ['', 'é🙂', '{"content":"needle\\n\\\"é🙂"}\n{"next":true}', 'x'.repeat(1048576)]) assert.equal(encode(text), expected(text));
  assert.equal(encode('}\n{'), 1); assert.equal(encode('}') + encode('{'), 2);
  assert.throws(() => encode('x'.repeat(1048577)), { code: 'resource_exhausted' });
  // Retained registry uses verified snapshots; changed files require a new pin on restart.
  await writeFile(config.path, 'throw new Error("tamper");');
  await writeFile(config.assets[0].path, '[]');
  assert.equal(encode('}\n{'), 1);
  assert.throws(() => loadTokenizers(JSON.stringify(config)));
}));
test('strict server configuration, digest binding, missing files and no ambient module resolution', () => installed(async (config, root) => {
  assert.equal(loadTokenizers(undefined).size, 0);
  const invalid = [null, [], {}, { ...config, id: 'bare' }, { ...config, id: 'x@sha256:' + '0'.repeat(64) },
    { ...config, path: 'relative.cjs' }, { ...config, extra: true }, { ...config, assets: undefined },
    { ...config, assets: [] }, { ...config, assets: [...config.assets, ...config.assets] },
    { ...config, assets: [{ ...config.assets[0], extra: true }] }, { ...config, assets: [{ name: 'bad\0name', path: config.path }] },
    { ...config, path: root + '/missing' }, { ...config, path: root },
    { ...config, assets: [{ name: 'vocabulary', path: root + '/missing' }] }];
  for (const value of invalid) assert.throws(() => loadTokenizers(JSON.stringify(value)));
  assert.throws(() => loadTokenizers('{'));
  for (const code of ['require("node:fs")', 'process.env', 'module.exports = {}', 'module.exports.createEncoder = () => 3']) {
    const other = await install(root, Buffer.from(code));
    assert.throws(() => loadTokenizers(JSON.stringify(other)));
  }
  const long = await install(root); await truncate(long.path, 16 * 1024 * 1024 + 1);
  assert.throws(() => loadTokenizers(JSON.stringify(long)));
}));
test('encoder count rejects negative, fractional, nonfinite, unsafe, nonnumeric and async outputs', () => installed(async (_config, root) => {
  for (const expression of ['-1', '0.5', 'NaN', 'Infinity', 'Number.MAX_SAFE_INTEGER + 1', '"1"', 'undefined', 'Promise.resolve(1)']) {
    const config = await install(root, Buffer.from(`module.exports.createEncoder = () => text => text === '' ? 0 : ${expression};`));
    const encode = loadTokenizers(JSON.stringify(config)).get(config.id);
    assert.throws(() => encode('nonempty'), { code: 'invalid_budget' });
  }
  for (const expression of ['1', '-1', 'NaN']) {
    const config = await install(root, Buffer.from(`module.exports.createEncoder = () => () => ${expression};`));
    assert.throws(() => loadTokenizers(JSON.stringify(config)));
  }
}));
test('bundle lexical text cannot shadow the host input', () => installed(async (_config, root) => {
  const config = await install(root, Buffer.from("const text = ''; module.exports.createEncoder = () => value => new TextEncoder().encode(value).length;"));
  const encoder = loadTokenizers(JSON.stringify(config)).get(config.id);
  for (const value of ['', 'hello', 'é🙂', 'different input', '']) assert.equal(encoder(value), Buffer.byteLength(value));
}));
test('bundle lexical encode cannot intercept host encoder initialization', () => installed(async (_config, root) => {
  const config = await install(root, Buffer.from('const encode = value => new TextEncoder().encode(value).length; module.exports.createEncoder = () => encode;'));
  const encoder = loadTokenizers(JSON.stringify(config)).get(config.id);
  assert.equal(encoder('hello'), 5); assert.equal(encoder(''), 0);
}));
test('module and asset lexical bindings stay outside the factory and invocation bridge', () => installed(async (_config, root) => {
  const config = await install(root, Buffer.from(`
    const module = globalThis.module, assets = null, text = '', encode = null;
    const scope = null, invoke = null, count = null, config = null;
    module.exports.createEncoder = supplied => {
      if (!supplied.vocabulary || assets !== null) throw Error('wrong assets');
      return value => new TextEncoder().encode(value).length;
    };
  `));
  const encoder = loadTokenizers(JSON.stringify(config)).get(config.id);
  assert.equal(encoder('hello'), 5); assert.equal(encoder('é🙂'), 6);
}));
test('module execution, factory and cross-context encoder invocation retain VM timeouts', { timeout: 20000 }, () => installed(async (_config, root) => {
  for (const code of ['while (true) {}', 'module.exports.createEncoder = () => { while (true) {} };']) {
    const config = await install(root, Buffer.from(code));
    assert.throws(() => loadTokenizers(JSON.stringify(config)), { code: 'ERR_SCRIPT_EXECUTION_TIMEOUT' });
  }
  const config = await install(root, Buffer.from("module.exports.createEncoder = () => value => { if (value === '') return 0; while (true) {} };"));
  const encoder = loadTokenizers(JSON.stringify(config)).get(config.id);
  assert.throws(() => encoder('hello'), { code: 'ERR_SCRIPT_EXECUTION_TIMEOUT' });
  assert.equal(encoder(''), 0);
}));
test('provider is operator-configurable beyond the synthetic fixture', () => installed(async (_config, root) => {
  // Independent encoder ABI implementation: an explicit whitespace-word vocabulary.
  // Only an ABI test, never presented as a real model encoder.
  const config = await install(root, Buffer.from('module.exports.createEncoder = assets => { if (!assets.vocabulary) throw Error("missing"); return text => text ? text.split(/\\s+/u).length : 0; };'));
  const encoder = loadTokenizers(JSON.stringify(config)).get(config.id);
  assert.equal(encoder('independent provider'), 2);
  const bytes = await readFile(config.path), asset = await readFile(config.assets[0].path);
  assert.ok(config.id.endsWith(sha(JSON.stringify(['anamnesis-tokenizer-v1', sha(bytes), [['vocabulary', sha(asset)]]]))));
}));
