import { expect, spyOn, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm, stat, writeFile } from "node:fs/promises";
import { RpcClient } from "./client.ts";
import { ingestSource } from "./source.ts";
import { RpcRememberParams, type RpcIngestStatusParams } from "../../packages/protocol/src/rpc.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const epoch = "22222222-2222-4222-8222-222222222222";
const foreign = "33333333-3333-4333-8333-333333333333";
const ready = { data_incarnation: incarnation, storage: "available" };
// Machine-consumed stdout events of one run, captured without touching the adapter.
async function events(run: () => Promise<unknown>): Promise<{ event: string; [key: string]: unknown }[]> {
  const log = spyOn(console, "log").mockImplementation(() => {});
  try { await run(); return log.mock.calls.map(([line]) => JSON.parse(String(line))); }
  finally { log.mockRestore(); }
}
const hash = (value: string) => createHash("sha256").update(value).digest("hex");
const input = (index: number) => RpcRememberParams.parse({
  episode: { schema: "anamnesis.original-message/1", time: { value: "2026-09-09T00:00:00Z", precision: "second" },
    content: `source-${index}`, mass: 1, properties: { b: { z: 1, a: 2 } },
    origin: { source: "source-commit", session: "s", actor: "a", record: `source-${index}` } },
  source_revision: "v1", expected_previous_revision_key: null,
});
// Independent JSON property-list canonicalizer, not the source implementation.
function identity(params: RpcRememberParams): RpcIngestStatusParams {
  const value = { digest_version: 1, params };
  const keys = new Set<string>();
  const collect = (value: unknown) => {
    if (value && typeof value === "object") for (const [key, child] of Object.entries(value)) {
      if (!Array.isArray(value)) keys.add(key);
      collect(child);
    }
  };
  collect(value);
  const o = params.episode.origin;
  return { revision_key: hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision])),
    body_digest: hash(JSON.stringify(value, [...keys].sort())), data_incarnation: incarnation };
}
const committed = (params: RpcRememberParams, index = 0) => ({ state: "committed", ...identity(params),
  id: `01900000-0000-7000-8000-${String(index + 1).padStart(12, "0")}`, created: false, ingest_seq: index + 1 });
const spooled = (params: RpcRememberParams) => ({ state: "spooled", ...identity(params), fs_epoch: epoch, spool_seq: 1 });
const mock = (request: (method: string, params: unknown) => Promise<unknown>): RpcClient =>
  Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request });

async function fixture(run: (f: { source: string; cp: string; pending: string; records: RpcRememberParams[]; sourceHash: string }) => Promise<void>, count = 1) {
  const root = await mkdtemp("/tmp/ana-source-commit-");
  const source = root + "/source.jsonl", cp = root + "/checkpoint.json";
  const records = Array.from({ length: count }, (_, index) => input(index));
  const text = records.map(record => JSON.stringify(record)).join("\n") + "\n";
  await writeFile(source, text);
  try { await run({ source, cp, pending: cp + ".pending.json", records, sourceHash: hash(text) }); }
  finally { await rm(root, { recursive: true, force: true }); await expect(stat(root)).rejects.toHaveProperty("code", "ENOENT"); }
}
const saved = async (path: string) => JSON.parse(await readFile(path, "utf8"));
const initial = (sourceHash: string) => ({ version: 1, source_hash: sourceHash, data_incarnation: incarnation, next: 0, last: null });

test("source mutation during record consumption cannot advance checkpoint", () => fixture(async ({ source, cp, records, sourceHash }) => {
  const client = mock(async method => {
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "remember") {
      await writeFile(source, JSON.stringify(records[0]) + "\nchanged\n");
      return committed(records[0]!);
    }
    throw new Error(`unexpected ${method}`);
  });
  await expect(ingestSource(source, cp, client)).rejects.toThrow("source_changed");
  expect(await saved(cp)).toEqual(initial(sourceHash));
}));

test("spooled is not source completion: pending exists before send and checkpoint stays zero", () => fixture(async ({ source, cp, pending, records, sourceHash }) => {
  let pendingAtSend: unknown;
  const client = mock(async (method) => {
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "remember") {
      pendingAtSend = await saved(pending).catch(error => ({ error: error.code }));
      return spooled(records[0]!);
    }
    throw new Error(`unexpected ${method}`);
  });
  const error = await ingestSource(source, cp, client).then(() => null, error => error);
  expect(await saved(cp)).toEqual(initial(sourceHash));
  expect(error?.code).toBe("source_pending_spooled");
  expect(pendingAtSend).toEqual({ version: 1, source_hash: sourceHash, index: 0, params: records[0], identity: identity(records[0]!) });
  expect(await saved(pending)).toEqual(pendingAtSend);
}));

for (const state of ["spooled", "quarantined", "blocked"] as const) {
  test(`pending restart with ${state} never resends or advances`, () => fixture(async ({ source, cp, pending, records, sourceHash }) => {
    const work = { version: 1, source_hash: sourceHash, index: 0, params: records[0], identity: identity(records[0]!) };
    await writeFile(cp, JSON.stringify(initial(sourceHash)));
    await writeFile(pending, JSON.stringify(work));
    const before = await readFile(cp), beforePending = await readFile(pending);
    let sends = 0;
    const client = mock(async (method, params) => {
      if (method === "status") return ready;
      if (method === "remember") { sends++; return committed(records[0]!); }
      expect(params).toEqual(work.identity);
      return { state, ...work.identity, ...(state === "spooled" ? { fs_epoch: epoch, spool_seq: 1 } : {}) };
    });
    const error = await ingestSource(source, cp, client).then(() => null, error => error);
    expect(sends).toBe(0);
    expect(error?.code).toBe(`source_pending_${state}`);
    expect(await readFile(cp)).toEqual(before);
    expect(await readFile(pending)).toEqual(beforePending);
  }));
}

// UNKNOWN alone is not a licence to resend: the daemon must be the same
// installation and able to rule out a committed delivery (storage available)
// in the very answer that says unknown. The storage state ships inside the
// `ingest.status` reply: a `status` taken before storage went away (available,
// then the database is lost, then unknown) must not license the resend.
for (const [name, status, storage, failure, calls] of [
  ["storage unavailable", { data_incarnation: incarnation, storage: "unavailable" }, "unavailable", "source_pending_unknown", ["status", "ingest.status"]],
  ["storage lost after status", ready, "unavailable", "source_pending_unknown", ["status", "ingest.status"]],
  ["foreign incarnation", { data_incarnation: foreign, storage: "available" }, "available", "incarnation_mismatch", ["status"]],
] as const) {
  test(`unknown pending with ${name} is neither retired nor resent`, () => fixture(async ({ source, cp, pending, records, sourceHash }) => {
    const work = { version: 1, source_hash: sourceHash, index: 0, params: records[0], identity: identity(records[0]!) };
    await writeFile(cp, JSON.stringify(initial(sourceHash)));
    await writeFile(pending, JSON.stringify(work));
    const before = await readFile(cp), beforePending = await readFile(pending);
    const seen: string[] = [];
    const client = mock(async (method, params) => {
      seen.push(method);
      if (method === "status") return status;
      if (method === "remember") throw new Error("unexpected retransmission");
      expect(params).toEqual(work.identity);
      return { state: "unknown", ...work.identity, storage };
    });
    const error = await ingestSource(source, cp, client).then(() => null, error => error);
    expect(error?.message).toBe(failure);
    expect(seen).toEqual([...calls]);
    expect(await readFile(cp)).toEqual(before);
    expect(await readFile(pending)).toEqual(beforePending);
  }));
}

test("committed pending resolves through status, checkpoints exactly once, then reclaims pending", () => fixture(async ({ source, cp, pending, records, sourceHash }) => {
  const work = { version: 1, source_hash: sourceHash, index: 0, params: records[0], identity: identity(records[0]!) };
  await writeFile(cp, JSON.stringify(initial(sourceHash))); await writeFile(pending, JSON.stringify(work));
  const calls: string[] = [];
  const client = mock(async (method, params) => {
    calls.push(method);
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "ingest.status") { expect(params).toEqual(work.identity); return committed(records[0]!); }
    throw new Error("unexpected remember");
  });
  await ingestSource(source, cp, client);
  expect(await saved(cp)).toEqual({ ...initial(sourceHash), next: 1, last: work.identity });
  await expect(stat(pending)).rejects.toHaveProperty("code", "ENOENT");
  expect(calls).toEqual(["status", "ingest.status"]);
  await ingestSource(source, cp, client);
  expect(calls).toEqual(["status", "ingest.status", "status", "ingest.status"]);
}));

test("UNKNOWN reply retains pre-send identity/body; resume resends only once the daemon disowns it", () => fixture(async ({ source, cp, pending, records, sourceHash }) => {
  const work = { version: 1, source_hash: sourceHash, index: 0, params: records[0], identity: identity(records[0]!) };
  const calls: string[] = [], pendingAtSend: unknown[] = [];
  let lost = true;
  const client = mock(async (method, params) => {
    calls.push(method);
    if (method === "status") return ready;
    if (method === "remember") {
      pendingAtSend.push(await saved(pending));
      if (lost) throw Object.assign(new Error("lost reply"), { code: "outcome_unknown" });
      expect(params).toEqual(records[0]);
      return committed(records[0]!);
    }
    expect(params).toEqual(work.identity);
    return { state: "unknown", ...work.identity, storage: "available" };
  });
  await expect(ingestSource(source, cp, client)).rejects.toHaveProperty("code", "outcome_unknown");
  expect(await saved(cp)).toEqual(initial(sourceHash));
  expect(await saved(pending)).toEqual(work);
  lost = false;
  const emitted = await events(() => ingestSource(source, cp, client));
  expect(calls).toEqual(["status", "remember", "status", "ingest.status", "remember"]);
  expect(pendingAtSend).toEqual([work, work]);
  expect(emitted.filter(e => e.event === "pending_retired")).toEqual([{ event: "pending_retired", reason: "daemon_unknown", index: 0, identity: work.identity }]);
  expect(emitted.map(e => e.event)).toEqual(["pending_retired", "source_checkpoint", "source_complete"]);
  expect(await saved(cp)).toEqual({ ...initial(sourceHash), next: 1, last: work.identity });
  await expect(stat(pending)).rejects.toHaveProperty("code", "ENOENT");
}));

for (const attack of ["forged-next", "swapped-last"] as const) {
  test(`${attack} cannot skip a different immutable source line`, () => fixture(async ({ source, cp, records, sourceHash }) => {
    const checkpoint = { ...initial(sourceHash), next: attack === "forged-next" ? 3 : 1, last: identity(records[1]!) };
    await writeFile(cp, JSON.stringify(checkpoint)); const before = await readFile(cp);
    let lookups = 0, sends = 0;
    const client = mock(async method => {
      if (method === "status") return { data_incarnation: incarnation };
      if (method === "remember") { sends++; return committed(records[2]!, 2); }
      lookups++; return committed(records[1]!, 1);
    });
    await expect(ingestSource(source, cp, client)).rejects.toThrow("source_checkpoint_invalid");
    expect(lookups).toBe(0); expect(sends).toBe(0); expect(await readFile(cp)).toEqual(before);
  }, 3));
}

test("old checkpoint with spooled last is not accepted as committed skipped work", () => fixture(async ({ source, cp, records, sourceHash }) => {
  await writeFile(cp, JSON.stringify({ ...initial(sourceHash), next: 1, last: identity(records[0]!) }));
  const before = await readFile(cp);
  const client = mock(async method => method === "status" ? { data_incarnation: incarnation } : spooled(records[0]!));
  await expect(ingestSource(source, cp, client)).rejects.toThrow("source_checkpoint_spooled");
  expect(await readFile(cp)).toEqual(before);
}));

test("crash after checkpoint publish reclaims only its matching committed pending", () => fixture(async ({ source, cp, pending, records, sourceHash }) => {
  const work = { version: 1, source_hash: sourceHash, index: 0, params: records[0], identity: identity(records[0]!) };
  const checkpoint = { ...initial(sourceHash), next: 1, last: work.identity };
  await writeFile(cp, JSON.stringify(checkpoint)); await writeFile(pending, JSON.stringify(work));
  const client = mock(async method => {
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "ingest.status") return committed(records[0]!);
    throw new Error("unexpected retransmission");
  });
  await ingestSource(source, cp, client);
  expect(await saved(cp)).toEqual(checkpoint); await expect(stat(pending)).rejects.toHaveProperty("code", "ENOENT");
}));
