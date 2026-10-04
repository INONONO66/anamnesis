import { expect, test } from "bun:test";
import { mkdtemp, readFile, readdir, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { MaterializationState, type MaterializationStateEntry } from "./materialization-state.ts";

const generation = "018f5b5e-7b1e-7abc-8def-123456789010";
const otherGeneration = "018f5b5e-7b1e-7abc-8def-123456789011";
const source = "018f5b5e-7b1e-7abc-8def-123456789012";
const otherSource = "018f5b5e-7b1e-7abc-8def-123456789013";
const operation = "018f5b5e-7b1e-7abc-8def-123456789014";
const second = "018f5b5e-7b1e-7abc-8def-123456789015";
const third = "018f5b5e-7b1e-7abc-8def-123456789016";
const fact = "018f5b5e-7b1e-7abc-8def-123456789017";
const link = "018f5b5e-7b1e-7abc-8def-123456789018";

function entry(overrides: Partial<MaterializationStateEntry> = {}): MaterializationStateEntry {
  return { request_digest: "a".repeat(64), occurrence_key: "occurrence-one", generation_id: generation,
    source_episode_id: source, source_ingest_seq: 1, fact_ids: [fact],
    result: { created: true, fact_id: fact, link_id: link }, ...overrides };
}

async function withFile(run: (path: string) => Promise<void>): Promise<void> {
  const directory = await mkdtemp(join(tmpdir(), "materialization-state-"));
  try { await run(join(directory, "materialization-state.json")); }
  finally { await rm(directory, { recursive: true }); }
}

test("loads existing materialization operations and returns isolated copies", async () => {
  await withFile(async path => {
    const record = entry();
    await writeFile(path, JSON.stringify({ version: 1, operations: { [operation]: record } }));
    const state = new MaterializationState(path);

    const found = await state.get(operation);
    expect(found).toEqual(record);
    expect(await state.byOccurrence(record.occurrence_key)).toEqual(record);
    expect(await state.get(second)).toBeUndefined();
    expect(await state.byOccurrence("absent")).toBeUndefined();
    if (!found) throw new Error("missing operation");
    found.fact_ids.push(second);
    expect((await state.get(operation))?.fact_ids).toEqual([fact]);
  });
});

test("serial writes roundtrip through one complete file", async () => {
  await withFile(async path => {
    const state = new MaterializationState(path);
    const first = entry(), next = entry({ occurrence_key: "occurrence-two", fact_ids: [],
      result: { created: false, refused: "echo_lineage_unavailable" } });
    await Promise.all([state.set(operation, first), state.set(second, next)]);

    expect(JSON.parse(await readFile(path, "utf8"))).toEqual({ version: 1, operations: { [operation]: first, [second]: next } });
    expect(await readdir(join(path, ".."))).toEqual(["materialization-state.json"]);
    expect(await new MaterializationState(path).list()).toEqual([[operation, first], [second, next]]);
    first.fact_ids.push(third);
    expect((await state.get(operation))?.fact_ids).toEqual([fact]);
  });
});

test("rejects malformed envelopes, operation IDs, entries, and result boundaries", async () => {
  await withFile(async path => {
    const valid = entry();
    for (const file of [
      { version: 2, operations: {} },
      { version: 1, operations: {}, extra: true },
      { version: 1, operations: { invalid: valid } },
      { version: 1, operations: { [operation]: { ...valid, extra: true } } },
      { version: 1, operations: { [operation]: { ...valid, request_digest: "invalid" } } },
      { version: 1, operations: { [operation]: { ...valid, source_ingest_seq: 0 } } },
      { version: 1, operations: { [operation]: { ...valid, fact_ids: Array(65).fill(fact) } } },
      { version: 1, operations: { [operation]: { ...valid, result: { created: false, facts: -1, refused: [] } } } },
    ]) {
      await writeFile(path, JSON.stringify(file));
      await expect(new MaterializationState(path).list()).rejects.toThrow();
    }
    const state = new MaterializationState();
    await expect(state.set("invalid", valid)).rejects.toThrow();
    await expect(state.set(operation, { ...valid, source_ingest_seq: 0 })).rejects.toThrow();
    expect(await state.list()).toEqual([]);
  });
});

test("deleteSource removes only matching generation and source", async () => {
  await withFile(async path => {
    const state = new MaterializationState(path);
    const sameSource = entry({ occurrence_key: "same-source" });
    const differentSource = entry({ occurrence_key: "different-source", source_episode_id: otherSource });
    const differentGeneration = entry({ occurrence_key: "different-generation", generation_id: otherGeneration });
    await state.set(operation, sameSource);
    await state.set(second, differentSource);
    await state.set(third, differentGeneration);

    await state.deleteSource(generation, source);

    expect(await state.get(operation)).toBeUndefined();
    expect(await new MaterializationState(path).list()).toEqual([[second, differentSource], [third, differentGeneration]]);
  });
});

test("a replacement intent discards an abandoned operation for the same occurrence", async () => {
  const state = new MaterializationState();
  const stale = entry();
  const committed = entry({ fact_ids: [], result: { created: false, refused: "echo_lineage_unavailable" } });
  await state.set(operation, stale);

  await state.set(second, committed);

  expect(await state.get(operation)).toBeUndefined();
  expect(await state.byOccurrence(committed.occurrence_key)).toEqual(committed);
  expect(await state.list()).toEqual([[second, committed]]);
});
