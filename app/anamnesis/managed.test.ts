import assert from "node:assert/strict";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { managedOperation, managedStatus, processIdentity } from "./managed.ts";

test("managed lifecycle journals intent in private state and lock files", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-managed-red-"));
  try {
    const identity = await processIdentity(process.pid);
    assert.match(identity, /^[a-f0-9]{64}$/);
    const started = await managedOperation(root, "start", { pid: process.pid, identity });
    assert.equal(started.intent, "start");
    const status = await managedStatus(root);
    assert.equal("intent" in status ? status.intent : status.state, "start");
    await managedOperation(root, "restart", { pid: process.pid, identity });
    const journal = JSON.parse(await readFile(join(root, "managed.state.json"), "utf8"));
    assert.equal(journal.intent, "restart");
    assert.equal((await managedOperation(root, "stop", { pid: process.pid, identity })).intent, "stop");
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("managed lifecycle refuses stale identity and unknown stop target", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-managed-red-"));
  try {
    await assert.rejects(managedOperation(root, "stop", { pid: process.pid, identity: "0".repeat(64) }), /identity/);
    await assert.rejects(managedOperation(root, "stop", { pid: process.pid + 1000000, identity: "0".repeat(64) }), /identity/);
  } finally { await rm(root, { recursive: true, force: true }); }
});
