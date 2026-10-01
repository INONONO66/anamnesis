import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import fs from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { runtimeTimed, timingHash, timingLog } from "./timing.ts";

const sha256 = (text: string) => createHash("sha256").update(text).digest("hex");

async function withRoot(run: (root: string) => Promise<void>): Promise<void> {
  const root = await fs.mkdtemp(join(tmpdir(), "ana-timing-"));
  try { await run(root); } finally { await fs.rm(root, { recursive: true, force: true }); }
}
const events = async (path: string) => (await fs.readFile(path, "utf8")).trimEnd().split("\n").map(line => JSON.parse(line) as Record<string, unknown>);

test("timing log numbers events, hashes string ids and rotates into one older segment", () => withRoot(async root => {
  const path = join(root, "timing.jsonl");
  const sink = timingLog(path, 1024);
  sink({ layer: "client", event: "send", method: "remember", id: "req-1", bytes: 12 });
  sink({ layer: "client", event: "reply", method: "remember", id: 7, elapsedMs: 3 });
  const first = await events(path);
  assert.deepEqual(first.map(event => [event.eventSequence, event.layer, event.event, event.method, event.id, event.pid]),
    [[1, "client", "send", "remember", sha256("req-1"), process.pid], [2, "client", "reply", "remember", 7, process.pid]]);
  assert.equal((await fs.stat(path)).mode & 0o777, 0o600);
  for (let i = 0; i < 4; i++) sink({ layer: "daemon", event: "tick", operation: "x".repeat(300) });
  const kept = [...await events(path + ".1"), ...await events(path)].map(event => event.eventSequence);
  // Each tick line is ~434 bytes against the 1024 budget: 4 and 5 share the rotated segment, 6 opened the current one.
  assert.deepEqual(kept, [4, 5, 6]);
  assert.throws(() => sink({ layer: "daemon", event: "huge", operation: "y".repeat(1024) }), /exceeds segment budget/);
}));

test("timingHash is plain sha256 hex", () => {
  assert.equal(timingHash("abc"), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
});

test("a line exactly at the budget is appended and rotates the segment only once it would overflow", () => withRoot(async root => {
  const now = performance.now;
  performance.now = () => 1000;
  try {
    const probe = join(root, "probe.jsonl");
    timingLog(probe)({ layer: "neo4j", event: "query" });
    const budget = (await fs.stat(probe)).size;
    const path = join(root, "timing.jsonl");
    const sink = timingLog(path, budget);
    sink({ layer: "neo4j", event: "query" });
    await assert.rejects(fs.stat(path + ".1"), { code: "ENOENT" });
    assert.equal((await fs.stat(path)).size, budget);
    sink({ layer: "neo4j", event: "query" });
    assert.deepEqual([(await events(path + ".1")).map(event => event.eventSequence), (await events(path)).map(event => event.eventSequence)], [[1], [2]]);
    assert.throws(() => timingLog(join(root, "tight.jsonl"), budget - 1)({ layer: "neo4j", event: "query" }), /exceeds segment budget/);
  } finally {
    performance.now = now;
  }
}));

test("timing log resumes the size of an existing segment before rotating, and only a missing segment is tolerated", () => withRoot(async root => {
  const path = join(root, "timing.jsonl");
  await fs.writeFile(path, "previous\n".repeat(50));
  timingLog(path, 500)({ layer: "neo4j", event: "query" });
  assert.equal(await fs.readFile(path + ".1", "utf8"), "previous\n".repeat(50));
  assert.deepEqual((await events(path)).map(event => [event.eventSequence, event.event]), [[1, "query"]]);
  assert.throws(() => timingLog(join(path, "under-a-file.jsonl")), { code: "ENOTDIR" });
}));

test("runtimeTimed is a pass-through without a timing path and records start/complete/failed with one", () => withRoot(async root => {
  assert.equal(await runtimeTimed("noop", async () => 41 + 1), 42);
  const path = join(root, "runtime.jsonl");
  const configured = process.env["ANAMNESIS_G006_TIMING_PATH"];
  process.env["ANAMNESIS_G006_TIMING_PATH"] = path;
  const specifier = "./timing.ts?daemon-timing";
  let timed: typeof import("./timing.ts");
  try { timed = await import(specifier); }
  finally { if (configured === undefined) delete process.env["ANAMNESIS_G006_TIMING_PATH"]; else process.env["ANAMNESIS_G006_TIMING_PATH"] = configured; }
  const now = performance.now;
  let tick = 1000;
  performance.now = () => tick++;
  let result: string;
  try {
    result = await timed.timingContext.run({ method: "recall", id: "call-1", connection: 3 },
      () => timed.runtimeTimed("engine.recall", async () => "ok", sha256("q")));
    await assert.rejects(timed.runtimeTimed("engine.remember", async () => { throw new Error("boom"); }), /boom/);
  } finally {
    performance.now = now;
  }
  assert.equal(result, "ok");
  const recorded = await events(path);
  assert.deepEqual(recorded.map(event => [event.event, event.operation, event.method, event.connection, event.layer]),
    [["start", "engine.recall", "recall", 3, "runtime"], ["complete", "engine.recall", "recall", 3, "runtime"],
      ["start", "engine.remember", undefined, undefined, "runtime"], ["failed", "engine.remember", undefined, undefined, "runtime"]]);
  assert.equal(recorded[0]!.id, sha256("call-1"));
  assert.equal(recorded[0]!.hash, sha256("q"));
  assert.equal(recorded[3]!.hash, sha256("Error: boom"));
  // started, start.monotonicMs, elapsed, complete.monotonicMs: two ticks between the clock reads that bound the action.
  assert.deepEqual(recorded.map(event => event.elapsedMs), [undefined, 2, undefined, 2]);
}));
