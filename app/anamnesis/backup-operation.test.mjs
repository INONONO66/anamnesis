import { test } from "node:test";
import { once } from "node:events";
import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { join } from "node:path";
import { syncBuiltinESMExports } from "node:module";
import { api, fixture, canonical, hash, id, secondId, validation, command, proof, advanceTo, child } from "./backup-operation.fixture.mjs";
import { fixture as archiveFixture } from "./archive-manifest.fixture.mjs";

async function acquire(_t, f, options = { validateProof: validation }) {
  const { acquireBackupOperation } = await api(), journal = await acquireBackupOperation(f.root, options);
  f.disposers.push(() => journal.release()); return journal;
}
test("missing module/API RED: exact unknown status and durable PREPARE retry", async t => {
  const f = await fixture(t), j = await acquire(t, f);
  assert.deepEqual(await j.status(id), { status: "unknown", operation_id: id, reason: "not_found" });
  const first = await j.begin(f.body), bytes = await fs.readFile(f.state);
  assert.equal(first.status, "pending"); assert.equal(first.state.phase, "PREPARE"); assert.equal(first.state.version, 1);
  assert.equal(first.state.body_sha256, hash(f.body));
  assert.deepEqual(await j.begin(f.body), first); assert.deepEqual(await fs.readFile(f.state), bytes);
  assert.equal((await j.status(secondId)).status, "unknown");
  assert.deepEqual(await j.recover(), first);
  assert.equal(await fs.readFile(join(f.root, "unrelated"), "utf8"), "preserved");
});
test("finite canonical body and safe identity rejection does not create journal/destination", async t => {
  const f = await fixture(t), j = await acquire(t, f), body = JSON.parse(f.body);
  const bad = [f.body + "\n", f.body.replace('"operation_id":', '"operation_id":"x","operation_id":'), "[".repeat(1000), Buffer.from([255])];
  for (const mutate of [b => b.extra = 1, b => b.operation_id = id.replace("7000", "4000"), b => b.source.ino = "0", b => b.destination.path = f.root, b => b.destination.path = f.destination + "/../escape", b => b.destination.path = f.destination + "\\escape", b => b.source.path = "relative", b => b.destination.parent_dev = "NaN"]) {
    const b = structuredClone(body); mutate(b); bad.push(canonical(b));
  }
  for (const b of bad) await assert.rejects(j.begin(b));
  await assert.rejects(fs.lstat(f.state), { code: "ENOENT" }); await assert.rejects(fs.lstat(f.destination), { code: "ENOENT" });
});
test("existing complete or partial destination and restore journal are never reused", async t => {
  const f = await fixture(t), j = await acquire(t, f);
  await fs.mkdir(f.destination); await fs.writeFile(join(f.destination, "partial"), "not ours");
  await assert.rejects(j.begin(f.body), { code: "destination_exists" });
  assert.equal(await fs.readFile(join(f.destination, "partial"), "utf8"), "not ours");
  await fs.rm(f.destination, { recursive: true }); await fs.writeFile(f.root + "-restore.state", "retained restore");
  await assert.rejects(j.begin(f.body), { code: "restore_pending" });
});
test("CAS, body conflicts, stale retries and concurrent same-owner calls preserve exact persisted bytes", async t => {
  const f = await fixture(t), j = await acquire(t, f); await j.begin(f.body); await fs.mkdir(f.destination, { mode: 0o700 });
  const p = await proof(f, 1, "cutoff"), c = command(1, "PREPARE", "CUTOFF", null, p);
  const results = await Promise.all([j.advance(f.body, c), j.advance(f.body, c)]);
  assert.deepEqual(results[0], results[1]); const before = await fs.readFile(f.state);
  const wrong = JSON.parse(f.body); wrong.operation_id = secondId;
  for (const call of [() => j.begin(canonical(wrong)), () => j.advance(f.body, command(1, "CUTOFF", "STOPPING", "stop_database")), () => j.advance(f.body, command(1, "PREPARE", "STOPPING", "stop_database")), () => j.advance(f.body, c + " ")]) await assert.rejects(call());
  assert.deepEqual(await fs.readFile(f.state), before);
  const next = await j.advance(f.body, command(2, "CUTOFF", "STOPPING", "stop_database"));
  assert.deepEqual(await j.advance(f.body, c), next); // Old exact retry acknowledges latest retained state.
  assert.deepEqual(await j.begin(f.body), next);
});
test("all concrete phases preserve evidence and write-ahead intent, marker-last completion is externally observed", async t => {
  const f = await fixture(t), j = await acquire(t, f);
  let result = await advanceTo(f, j, "PUBLISH");
  assert.equal(result.state.phase, "COPYING"); assert.equal(result.state.intent, "publish_complete");
  assert.equal(result.state.history.filter(e => e.proof).length, 5);
  await assert.rejects(fs.lstat(join(f.destination, "backup.complete")), { code: "ENOENT" });
  // Future orchestrator alone publishes marker; journal checks the exact bytes.
  const archive = await archiveFixture(t);
  for (const name of await fs.readdir(archive.root)) await fs.cp(join(archive.root, name), join(f.destination, name), { recursive: true, errorOnExist: true, force: false });
  const marker = await fs.readFile(join(f.destination, "backup.complete"));
  await fs.rm(join(f.destination, "backup.complete"));
  const p = await proof(f, result.state.version, "completion_published");
  const c = command(result.state.version, "COPYING", "COMPLETE", null, p);
  await assert.rejects(j.advance(f.body, c));
  await fs.writeFile(join(f.destination, "backup.complete"), marker, { mode: 0o600 });
  result = await j.advance(f.body, c);
  assert.equal(result.status, "terminal"); assert.equal(result.state.phase, "COMPLETE");
  assert.deepEqual(await j.advance(f.body, c), result);
  assert.equal((await j.recover()).recovery.action, "none");
  t.diagnostic(JSON.stringify(result));
});
for (const phase of ["PREPARE", "CUTOFF", "STOPPING", "DB_STOPPED", "DUMPED", "DB_STARTED", "COPYING"]) test(`recovery and FAILED retain ${phase} distinctions without executing effects`, async t => {
  const f = await fixture(t), j = await acquire(t, f); const current = await advanceTo(f, j, phase);
  const before = await fs.readFile(f.state), recovered = await j.recover();
  assert.deepEqual(await fs.readFile(f.state), before);
  assert.equal(recovered.recovery.action, ["DB_STARTED", "COPYING"].includes(phase) ? "inspect_archive_before_resume" : "inspect_database_before_gate_release");
  assert.equal(recovered.recovery.preserve_dump, ["DUMPED", "DB_STARTED", "COPYING"].includes(phase));
  assert.equal(recovered.recovery.may_release_gate, false);
  const failed = await j.advance(f.body, command(current.state.version, phase, "FAILED", null, null, { code: "io_error", effect: "unknown" }));
  assert.equal(failed.status, "terminal"); assert.equal(failed.recovery.failed_from, phase);
  assert.equal(failed.recovery.action, recovered.recovery.action);
  await assert.rejects(j.advance(f.body, command(failed.state.version, "FAILED", "PREPARE")));
});
test("caller booleans, absent validator, corrupted/wrong-operation proof cannot assert dump or health", async t => {
  const f = await fixture(t), j = await acquire(t, f); const current = await advanceTo(f, j, "DB_STOPPED");
  const before = await fs.readFile(f.state), p = await proof(f, current.state.version, "dump_verified");
  for (const malformed of [true, { ...p, healthy: true }, { ...p, sha256: "f".repeat(64) }, { ...p, path: p.path.replace(id, secondId) }, { ...p, path: "../escape" }, { ...p, bytes: Infinity }]) await assert.rejects(j.advance(f.body, command(current.state.version, "DB_STOPPED", "DUMPED", "start_database", malformed)));
  assert.deepEqual(await fs.readFile(f.state), before);
  await fs.writeFile(join(f.root, p.path), canonical({ success: true }));
  p.bytes = (await fs.stat(join(f.root, p.path))).size; p.sha256 = hash(await fs.readFile(join(f.root, p.path)));
  await assert.rejects(j.advance(f.body, command(current.state.version, "DB_STOPPED", "DUMPED", "start_database", p)));
});
test("observations cannot proceed without trusted validator", async t => {
  const f = await fixture(t), j = await acquire(t, f, {}); await j.begin(f.body); await fs.mkdir(f.destination, { mode: 0o700 });
  await assert.rejects(j.advance(f.body, command(1, "PREPARE", "CUTOFF", null, await proof(f, 1, "cutoff"))), { code: "proof_required" });
});
for (const outcome of ["FAILED", "COMPLETE"]) for (const fault of ["write", "sync", "rename", "directory-sync"]) test(`real filesystem ${fault} fault during ${outcome} returns unknown or prior state`, async t => {
  const f = await fixture(t), j = await acquire(t, f);
  let c;
  if (outcome === "FAILED") { await j.begin(f.body); c = command(1, "PREPARE", "FAILED", null, null, { code: "io_error", effect: "unknown" }); }
  else {
    const current = await advanceTo(f, j, "PUBLISH"), archive = await archiveFixture(t);
    for (const name of await fs.readdir(archive.root)) await fs.cp(join(archive.root, name), join(f.destination, name), { recursive: true, errorOnExist: true, force: false });
    c = command(current.state.version, "COPYING", "COMPLETE", null, await proof(f, current.state.version, "completion_published"));
  }
  const prior = await fs.readFile(f.state); let injected = 0, temp, renamed = false;
  const original = { open: fs.open, rename: fs.rename };
  const fail = () => { injected++; throw Object.assign(new Error(`injected ${fault}`), { code: "EIO" }); };
  try {
    t.mock.method(fs, "open", async (...args) => {
      const file = await original.open(...args);
      if (typeof args[0] === "string" && args[0].startsWith(f.state + ".") && args[1] === "wx") {
        temp = args[0]; const write = file.writeFile.bind(file), sync = file.sync.bind(file);
        file.writeFile = async (...a) => { if (fault === "write") { await write('{"partial":'); fail(); } return write(...a); };
        file.sync = async () => { if (fault === "sync") fail(); return sync(); };
      }
      if (args[0] === f.root && args[1] === "r" && fault === "directory-sync" && renamed) file.sync = async () => fail();
      return file;
    });
    t.mock.method(fs, "rename", async (...args) => { if (args[1] === f.state && fault === "rename") fail(); await original.rename(...args); if (args[1] === f.state) renamed = true; });
    syncBuiltinESMExports();
    await assert.rejects(j.advance(f.body, c), { code: "outcome_unknown" });
    assert.equal((await j.status(id)).status, "unknown");
  } finally { t.mock.restoreAll(); syncBuiltinESMExports(); }
  assert.equal(injected, 1); await assert.rejects(fs.lstat(temp), { code: "ENOENT" });
  if (fault !== "directory-sync") assert.deepEqual(await fs.readFile(f.state), prior);
  const recovered = await j.recover();
  assert.equal(recovered.state.phase, fault === "directory-sync" ? outcome : JSON.parse(prior).phase);
  // Only explicit recovery after a fresh successful file+directory fsync can
  // acknowledge the rename that the failed call correctly reported UNKNOWN.
  t.diagnostic(JSON.stringify({ fault, outcome, recovered }));
});
test("two processes contend, then exact retry reconciles persisted state after lock release", async t => {
  const f = await fixture(t), a = await child(t, f);
  const ready = a.wait("ready"); a.worker.send("go"); const saved = await ready;
  const { acquireBackupOperation } = await api(); await assert.rejects(acquireBackupOperation(f.root, { validateProof: validation }), { code: "operation_locked" });
  const retry = a.wait("retried"); a.worker.send("retry"); assert.deepEqual((await retry).result, saved.result);
  const released = a.wait("released"); a.worker.send("release"); await released; await a.closed;
  const b = await child(t, f); const again = b.wait("ready"); b.worker.send("go"); assert.deepEqual((await again).result, saved.result);
  const done = b.wait("released"); b.worker.send("release"); await done;
});
for (const stage of ["file-sync", "rename", "ack"]) test(`SIGKILL after exact ${stage} event: recover discovers journal, never resends effects`, async t => {
  const f = await fixture(t), a = await child(t, f, "crash", stage);
  const persisted = a.wait("persisted"); a.worker.send("go"); const event = await persisted;
  assert.equal(event.stage, stage); const exited = once(a.worker, "exit"); assert.equal(a.worker.kill("SIGKILL"), true); const [, signal] = await exited; assert.equal(signal, "SIGKILL"); assert.equal(a.worker.signalCode, "SIGKILL");
  const j = await acquire(t, f), result = await j.recover();
  assert.equal(result.status, stage === "file-sync" ? "unknown" : "pending");
  if (stage !== "file-sync") assert.equal(result.state.phase, "PREPARE");
  else { assert.equal(result.reason, "interrupted_write"); await assert.rejects(j.begin(f.body), { code: "stale_state" }); }
  await assert.rejects(fs.lstat(f.destination), { code: "ENOENT" });
  assert.equal(await fs.readFile(join(f.root, "unrelated"), "utf8"), "preserved");
  t.diagnostic(JSON.stringify({ stage, result }));
});
test("malformed/incomplete stale lock rejects deterministically, live PID is never stolen", async t => {
  const f = await fixture(t), { acquireBackupOperation } = await api();
  await fs.mkdir(f.lock, { mode: 0o700 });
  await assert.rejects(acquireBackupOperation(f.root), { code: "stale_lock" });
  await fs.writeFile(join(f.lock, "owner.json"), canonical({ format: "anamnesis.operation-lock/1", pid: process.pid, nonce: "a".repeat(64), root: f.root }), { mode: 0o600 });
  await assert.rejects(acquireBackupOperation(f.root), { code: "operation_locked" });
});
test("journal symlinks, hardlinks, tampering and lost ownership fail closed", async t => {
  const f = await fixture(t), j = await acquire(t, f); await j.begin(f.body);
  const prior = await fs.readFile(f.state), outside = join(f.owner, "outside"); await fs.writeFile(outside, prior, { mode: 0o600 });
  await fs.rm(f.state); await fs.symlink(outside, f.state);
  assert.equal((await j.status(id)).status, "unknown"); await assert.rejects(j.begin(f.body));
  await fs.rm(f.state); await fs.link(outside, f.state); assert.equal((await j.status(id)).status, "unknown");
  await fs.rm(f.state); await fs.writeFile(f.state, prior, { mode: 0o600 });
  const parsed = JSON.parse(prior); parsed.version = 999; await fs.writeFile(f.state, canonical(parsed));
  assert.equal((await j.status(id)).status, "unknown"); await assert.rejects(j.begin(f.body));
  await fs.writeFile(f.state, prior);
  const ownerPath = join(f.lock, "owner.json"), owner = await fs.readFile(ownerPath);
  const claim = JSON.parse(owner); claim.nonce = "b".repeat(64); await fs.writeFile(ownerPath, canonical(claim));
  await assert.rejects(j.advance(f.body, command(1, "PREPARE", "FAILED", null, null, { code: "io_error", effect: "unknown" })), { code: "ownership_lost" });
  await fs.writeFile(ownerPath, owner); assert.deepEqual(await fs.readFile(outside), prior);
});

for (const phase of ["CUTOFF", "STOPPING", "DB_STOPPED", "DUMPED", "DB_STARTED", "COPYING", "PUBLISH"]) test(`child death preserves ${phase} and exact recovery classification`, async t => {
  const f = await fixture(t), a = await child(t, f, "phase", phase);
  const persisted = a.wait("persisted"); a.worker.send("go"); const event = await persisted;
  a.worker.kill("SIGKILL"); await a.closed;
  const saved = await fs.readFile(f.state), j = await acquire(t, f), recovered = await j.recover();
  assert.deepEqual(recovered, event.result); assert.deepEqual(await fs.readFile(f.state), saved);
  assert.deepEqual(await j.begin(f.body), recovered);
  await assert.rejects(fs.lstat(join(f.destination, "backup.complete")), { code: "ENOENT" });
  assert.equal(await fs.readFile(join(f.root, "unrelated"), "utf8"), "preserved");
  assert.equal(recovered.recovery.may_release_gate, false);
  if (phase === "PUBLISH") assert.equal(recovered.recovery.action, "inspect_publication");
  t.diagnostic(JSON.stringify({ killed_phase: phase, recovered }));
});

test("same UUID with changed canonical body and destination inode replacement are conflicts", async t => {
  const f = await fixture(t), j = await acquire(t, f); await advanceTo(f, j, "CUTOFF");
  const prior = await fs.readFile(f.state), changed = JSON.parse(f.body); changed.destination.path += "-other";
  await assert.rejects(j.begin(canonical(changed)), { code: "identity_conflict" });
  await fs.rename(f.destination, f.destination + ".retained"); await fs.mkdir(f.destination, { mode: 0o700 });
  assert.equal((await j.status(id)).status, "unknown");
  await assert.rejects(j.advance(f.body, command(2, "CUTOFF", "STOPPING", "stop_database")));
  assert.deepEqual(await fs.readFile(f.state), prior);
});

for (const kind of ["symlink", "hardlink", "shared", "oversized"]) test(`proof ${kind} cannot assert observed cutoff`, async t => {
  const f = await fixture(t), j = await acquire(t, f); await j.begin(f.body); await fs.mkdir(f.destination, { mode: 0o700 });
  const p = await proof(f, 1, "cutoff"), path = join(f.root, p.path), external = join(f.owner, "proof"), prior = await fs.readFile(f.state);
  await fs.writeFile(external, await fs.readFile(path), { mode: 0o600 });
  if (kind === "shared") await fs.chmod(path, 0o666);
  else if (kind === "oversized") { const file = await fs.open(path, "r+"); try { await file.truncate(1024 * 1024 + 1); } finally { await file.close(); } }
  else { await fs.rm(path); if (kind === "symlink") await fs.symlink(external, path); else await fs.link(external, path); }
  await assert.rejects(j.advance(f.body, command(1, "PREPARE", "CUTOFF", null, p)), { code: "unsafe_path" });
  assert.deepEqual(await fs.readFile(f.state), prior);
});

test("finite command schema rejects coerced failure enums and unknown/missing fields", async t => {
  const f = await fixture(t), j = await acquire(t, f); await j.begin(f.body); const prior = await fs.readFile(f.state);
  for (const mutate of [c => c.extra = true, c => delete c.expected_phase, c => c.expected_version = 1.5, c => c.expected_version = Number.MAX_SAFE_INTEGER, c => c.failure.code = ["io_error"], c => c.failure.effect = ["unknown"], c => c.failure.extra = true, c => c.phase = "invented"]) {
    const c = JSON.parse(command(1, "PREPARE", "FAILED", null, null, { code: "io_error", effect: "unknown" })); mutate(c);
    await assert.rejects(j.advance(f.body, canonical(c)));
  }
  assert.deepEqual(await fs.readFile(f.state), prior);
});

test("shared or linked roots and stale acquisition guards are not silently repaired", async t => {
  const f = await fixture(t), { acquireBackupOperation } = await api();
  const alias = join(f.owner, "alias"); await fs.symlink(f.root, alias);
  await assert.rejects(acquireBackupOperation(alias), { code: "unsafe_path" });
  await fs.chmod(f.root, 0o777); await assert.rejects(acquireBackupOperation(f.root), { code: "unsafe_path" }); await fs.chmod(f.root, 0o700);
  await fs.mkdir(f.lock + ".claim", { mode: 0o700 }); await fs.writeFile(join(f.lock + ".claim", "unrelated"), "retain");
  await assert.rejects(acquireBackupOperation(f.root), { code: "stale_lock" });
  assert.equal(await fs.readFile(join(f.lock + ".claim", "unrelated"), "utf8"), "retain");
});
