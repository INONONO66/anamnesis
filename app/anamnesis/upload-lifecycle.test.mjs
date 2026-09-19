import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import { syncBuiltinESMExports } from 'node:module';
import { createHash, randomUUID } from 'node:crypto';
import { resolve } from 'node:path';
import { pathToFileURL } from 'node:url';
const { Uploads } = await import(pathToFileURL(resolve(process.env.UPLOAD_BUNDLE_ROOT ?? '.omo/evidence/upload-lifecycle', 'upload-module.mjs')));
const HOUR = 3_600_000, MiB = 1024 * 1024;
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const metadata = bytes => ({ hash: hash(bytes), size: bytes.length, media_type: 'application/octet-stream' });
function clock() {
  let now = 0, next = 0; const timers = new Map(), queue = [];
  return { now: () => now, timers, queue,
    setTimeout(callback, delay) { const id = ++next; timers.set(id, { callback, at: now + delay }); return id; },
    clearTimeout(id) { timers.delete(id); },
    enqueue(job) { queue.push(job); },
    advance(value) { assert.ok(value >= now); now = value; for (const [id, timer] of timers) if (timer.at <= now) { timers.delete(id); timer.callback(); } },
    async drain() { while (queue.length) await queue.shift()(); },
  };
}
async function fixture(run, prepare = async () => {}) {
  const root = await fs.mkdtemp('/tmp/ana-upload-'), time = clock();
  const uploads = new Uploads(root + '/objects', root + '/uploads', { clock: time, enqueue: time.enqueue });
  try { await prepare(root); await uploads.init(); await run({ root, uploads, time, owner: {} }); }
  finally { try { await uploads.close(); } finally { await fs.rm(root, { recursive: true, force: true }); await assert.rejects(fs.stat(root), { code: 'ENOENT' }); } }
}
// Only the concrete handle returned for this owned upload is intercepted. Both
// paths perform a real prefix write; writeFile is the pre-fix implementation.
function failWrite(file, kind) {
  const write = file.write, writeFile = file.writeFile;
  file.write = async function (bytes, offset, length, position) {
    const result = await write.call(this, bytes, offset, Math.min(2, length), position);
    if (kind === 'short') return result;
    if (kind === 'zero') return { bytesWritten: 0, buffer: bytes };
    throw Object.assign(Error('owned upload disk full'), { code: 'ENOSPC' });
  };
  file.writeFile = async function (bytes) { await writeFile.call(this, bytes.subarray(0, 2)); throw Object.assign(Error('owned upload disk full'), { code: 'ENOSPC' }); };
  return () => { file.write = write; file.writeFile = writeFile; };
}

test('exact one-hour deadline invalidates a handle even when the expiry job is queued', () => fixture(async ({ root, uploads, time, owner }) => {
  const bytes = Buffer.from('ab'), a = await uploads.begin(owner, metadata(bytes));
  time.advance(HOUR - 1);
  assert.equal((await uploads.chunk(owner, a.upload_id, 0, bytes.subarray(0, 1))).next_seq, 1);
  time.advance(HOUR);
  await assert.rejects(uploads.chunk(owner, a.upload_id, 1, bytes.subarray(1)), { code: 'upload_not_found' });
  await assert.rejects(uploads.commit(owner, a.upload_id), { code: 'upload_not_found' });
  await time.drain();
  assert.deepEqual(await fs.readdir(root + '/uploads'), []);
  assert.equal(time.timers.size, 0);
}));

test('startup counts unrecognized surviving regular files without deleting them', () => fixture(async ({ root, uploads, owner }) => {
  assert.equal((await fs.stat(root + '/uploads/retain-user-data')).size, 1024 * MiB);
  await assert.rejects(uploads.begin(owner, metadata(Buffer.from('a'))), { code: 'resource_exhausted' });
}, async root => {
  await fs.mkdir(root + '/uploads', { mode: 0o700 });
  const file = await fs.open(root + '/uploads/retain-user-data', 'wx', 0o600);
  try { await file.truncate(1024 * MiB); } finally { await file.close(); }
}));

for (const kind of ['enospc', 'short', 'zero']) test(`real ${kind} partial chunk rolls back verified bytes, sequence, digest and reservations`, () => fixture(async ({ root, uploads, owner }) => {
  const bytes = Buffer.from('verified-tail'), a = await uploads.begin(owner, metadata(bytes));
  const prefix = bytes.subarray(0, 8), tail = bytes.subarray(8);
  await uploads.chunk(owner, a.upload_id, 0, prefix);
  const upload = uploads.uploads.get(a.upload_id), before = { received: upload.received, next: upload.next, digest: upload.hash.copy().digest('hex') };
  const restore = failWrite(upload.file, kind);
  try { await assert.rejects(uploads.chunk(owner, a.upload_id, 1, tail), { code: 'resource_exhausted' }); }
  finally { restore(); }
  assert.deepEqual(await fs.readFile(root + '/uploads/' + a.upload_id), prefix);
  assert.deepEqual({ received: upload.received, next: upload.next, digest: upload.hash.copy().digest('hex') }, before);
  assert.equal(upload.expected.size, bytes.length);
  assert.equal((await uploads.chunk(owner, a.upload_id, 1, tail)).next_seq, 2);
  assert.deepEqual(await uploads.commit(owner, a.upload_id), metadata(bytes));
  assert.deepEqual(await uploads.store.get(hash(bytes)), new Uint8Array(bytes));
  assert.deepEqual(await fs.readdir(root + '/uploads'), []);
}));

for (const operation of ['truncate', 'stat', 'sync']) test(`rollback ${operation} failure keeps charge and fails closed even after I/O recovers`, () => fixture(async ({ root, uploads, owner }) => {
  const bytes = Buffer.from('verified-tail'), a = await uploads.begin(owner, metadata(bytes));
  await uploads.begin(owner, metadata(bytes));
  await uploads.chunk(owner, a.upload_id, 0, bytes.subarray(0, 8));
  const upload = uploads.uploads.get(a.upload_id), restore = failWrite(upload.file, 'enospc');
  const original = upload.file[operation];
  upload.file[operation] = async () => { throw Object.assign(Error('owned rollback fault'), { code: 'EIO' }); };
  try { await assert.rejects(uploads.chunk(owner, a.upload_id, 1, bytes.subarray(8)), { code: 'resource_exhausted' }); }
  finally { restore(); upload.file[operation] = original; }
  assert.equal(upload.received, 8); assert.equal(upload.next, 1);
  assert.equal(upload.hash.copy().digest('hex'), hash(bytes.subarray(0, 8)));
  assert.equal(upload.actual, bytes.length); assert.equal(uploads.uploads.size, 2);
  const disk = await fs.readFile(root + '/uploads/' + a.upload_id);
  assert.deepEqual(disk, bytes.subarray(0, operation === 'truncate' ? 10 : 8));
  await assert.rejects(uploads.chunk(owner, a.upload_id, 1, bytes.subarray(8)), { code: 'resource_exhausted' });
  await assert.rejects(uploads.commit(owner, a.upload_id), { code: 'resource_exhausted' });
  await assert.rejects(uploads.begin(owner, metadata(bytes)), { code: 'resource_exhausted' });
  await uploads.disconnect(owner);
  assert.deepEqual(await fs.readdir(root + '/uploads'), []);
  assert.equal((await uploads.begin(owner, metadata(bytes))).state, 'uploading');
}));

function intercept(name, path, replace) {
  const original = fs[name];
  fs[name] = function (target, ...args) { return target === path ? replace(original, target, ...args) : original(target, ...args); };
  syncBuiltinESMExports();
  return () => { fs[name] = original; syncBuiltinESMExports(); };
}

test('all startup artifacts are charged before owned UUID deletion; old sessions never resume', async () => {
  const root = await fs.mkdtemp('/tmp/ana-upload-restart-'), id = randomUUID(), time = clock();
  const uploads = new Uploads(root + '/objects', root + '/uploads', { clock: time, enqueue: time.enqueue });
  await fs.mkdir(root + '/uploads', { mode: 0o700 });
  await fs.writeFile(root + '/uploads/' + id, 'old-prefix', { mode: 0o600 });
  await fs.writeFile(root + '/uploads/user-data', 'retained', { mode: 0o600 });
  let observed = false;
  const restore = intercept('unlink', root + '/uploads/' + id, async (original, path) => {
    observed = true;
    assert.equal(uploads.survivingCount, 2); assert.equal(uploads.survivingBytes, 18);
    await assert.rejects(uploads.begin({}, metadata(Buffer.from('x'))), { code: 'resource_exhausted' });
    return original(path);
  });
  try {
    await uploads.init(); assert.equal(observed, true);
    assert.equal(uploads.survivingCount, 1); assert.equal(uploads.survivingBytes, 8);
    assert.deepEqual(await fs.readdir(root + '/uploads'), ['user-data']);
    await assert.rejects(uploads.chunk({}, id, 0, Buffer.from('x')), { code: 'upload_not_found' });
    await assert.rejects(uploads.commit({}, id), { code: 'upload_not_found' });
    assert.equal((await uploads.begin({}, metadata(Buffer.from('new')))).state, 'uploading');
    await uploads.close(); assert.equal(await fs.readFile(root + '/uploads/user-data', 'utf8'), 'retained');
  } finally { restore(); await uploads.close(); await fs.rm(root, { recursive: true, force: true }); }
});

test('surviving count and bytes enforce exact independent global boundaries', () => fixture(async ({ root, uploads }) => {
  assert.equal(uploads.survivingCount, 16); assert.equal(uploads.survivingBytes, 16);
  const expected = { ...metadata(Buffer.from('x')), size: 64 * MiB };
  for (let i = 0; i < 15; i++) await uploads.begin({}, expected);
  // One byte over the remaining global declared+actual budget is rejected.
  await assert.rejects(uploads.begin({}, { ...expected, size: 64 * MiB - 15 }), { code: 'resource_exhausted' });
  await uploads.begin({}, { ...expected, size: 64 * MiB - 16 });
  assert.equal(uploads.uploads.size + uploads.survivingCount, 32);
  await assert.rejects(uploads.begin({}, metadata(Buffer.alloc(0))), { code: 'resource_exhausted' });
  assert.equal((await fs.readdir(root + '/uploads')).length, 32);
}, async root => {
  await fs.mkdir(root + '/uploads', { mode: 0o700 });
  for (let i = 0; i < 16; i++) await fs.writeFile(root + '/uploads/retained-' + i, 'x', { mode: 0o600 });
}));

test('connection count and declared bytes release at disconnect, never by transferring an id', () => fixture(async ({ uploads, owner }) => {
  const expected = { ...metadata(Buffer.from('x')), size: 64 * MiB };
  const a = await uploads.begin(owner, expected); await uploads.begin(owner, expected);
  await assert.rejects(uploads.begin(owner, expected), { code: 'resource_exhausted' });
  await assert.rejects(uploads.chunk({}, a.upload_id, 0, Buffer.from('x')), { code: 'upload_not_found' });
  await uploads.disconnect(owner);
  await assert.rejects(uploads.commit(owner, a.upload_id), { code: 'upload_not_found' });
  assert.equal((await uploads.begin(owner, expected)).state, 'uploading');
}));

test('expiry is autonomous, serially queued, bounded and cancelled at close', () => fixture(async ({ root, uploads, time, owner }) => {
  const a = await uploads.begin(owner, metadata(Buffer.from('x')));
  time.advance(1); const b = await uploads.begin(owner, metadata(Buffer.from('y')));
  assert.equal(time.timers.size, 1);
  time.advance(HOUR);
  assert.equal(time.queue.length, 1);
  assert.equal((await fs.readdir(root + '/uploads')).length, 2); // timer never mutates files
  await time.drain();
  assert.deepEqual(await fs.readdir(root + '/uploads'), [b.upload_id]);
  await assert.rejects(uploads.commit(owner, a.upload_id), { code: 'upload_not_found' });
  assert.equal(time.timers.size, 1);
  time.advance(HOUR + 1); assert.equal(time.queue.length, 1);
  await uploads.close(); await time.drain();
  assert.equal(time.timers.size, 0); assert.deepEqual(await fs.readdir(root + '/uploads'), []);
}));

for (const kind of ['symlink', 'directory', 'hardlink', 'wrong-mode']) test(`startup never recursively removes ${kind} artifacts`, async () => {
  const root = await fs.mkdtemp('/tmp/ana-upload-artifact-'), id = randomUUID();
  const uploads = new Uploads(root + '/objects', root + '/uploads');
  await fs.mkdir(root + '/uploads', { mode: 0o700 });
  const outside = root + '/outside'; await fs.writeFile(outside, 'user-data', { mode: 0o600 });
  const artifact = root + '/uploads/' + id;
  if (kind === 'symlink') await fs.symlink(outside, artifact);
  else if (kind === 'directory') { await fs.mkdir(artifact); await fs.writeFile(artifact + '/user-data', 'keep'); }
  else if (kind === 'hardlink') await fs.link(outside, artifact);
  else await fs.writeFile(artifact, 'keep', { mode: 0o644 });
  try {
    if (kind === 'symlink' || kind === 'directory') {
      await assert.rejects(uploads.init());
      await assert.rejects(uploads.begin({}, metadata(Buffer.from('x'))), { code: 'resource_exhausted' });
    } else { await uploads.init(); assert.equal(uploads.survivingCount, 1); }
    await uploads.close(); await fs.lstat(artifact);
    assert.equal(await fs.readFile(outside, 'utf8'), 'user-data');
  } finally { await uploads.close(); await fs.rm(root, { recursive: true, force: true }); }
});

test('open failure leaves no reservation or temp; cleanup unlink failure retains both until retry', () => fixture(async ({ root, uploads, owner }) => {
  // A real OS open failure, not a mock success without an artifact.
  await fs.chmod(root + '/uploads', 0o500);
  try { await assert.rejects(uploads.begin(owner, metadata(Buffer.from('x'))), { code: 'EACCES' }); }
  finally { await fs.chmod(root + '/uploads', 0o700); }
  assert.equal(uploads.uploads.size, 0); assert.deepEqual(await fs.readdir(root + '/uploads'), []);
  const a = await uploads.begin(owner, metadata(Buffer.from('x')));
  const restore = intercept('unlink', root + '/uploads/' + a.upload_id, async () => { throw Object.assign(Error('owned unlink fault'), { code: 'EIO' }); });
  try { await assert.rejects(uploads.disconnect(owner), { code: 'EIO' }); }
  finally { restore(); }
  assert.equal(uploads.uploads.size, 1); assert.equal((await fs.stat(root + '/uploads/' + a.upload_id)).size, 0);
  await assert.rejects(uploads.chunk(owner, a.upload_id, 0, Buffer.from('x')), { code: 'upload_not_found' });
  await uploads.disconnect(owner); assert.equal(uploads.uploads.size, 0);
}));

test('commit fsync failure cleans incomplete temp and never publishes metadata', () => fixture(async ({ root, uploads, owner }) => {
  const bytes = Buffer.from('payload'), a = await uploads.begin(owner, metadata(bytes));
  await uploads.chunk(owner, a.upload_id, 0, bytes);
  const upload = uploads.uploads.get(a.upload_id), sync = upload.file.sync;
  upload.file.sync = async () => { throw Object.assign(Error('owned fsync fault'), { code: 'ENOSPC' }); };
  try { await assert.rejects(uploads.commit(owner, a.upload_id), { code: 'ENOSPC' }); }
  finally { upload.file.sync = sync; }
  assert.equal(uploads.uploads.size, 0); assert.equal(await uploads.metadata(hash(bytes)), null);
  assert.deepEqual(await fs.readdir(root + '/uploads'), []);
}));
