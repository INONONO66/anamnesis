import { afterEach, describe, expect, test } from "bun:test";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { DurableSpool, type SpoolRecord } from "./spool.ts";
const roots: string[] = [];
afterEach(async () => { await Promise.all(roots.splice(0).map((root) => rm(root, { recursive: true, force: true }))); });
const record: SpoolRecord = { origin: "o", revision: "r", predecessor: null, body: { z: 1, a: "x" }, incarnation: "i" };
async function spool(): Promise<{ root: string; value: DurableSpool }> { const root = await mkdtemp(join(tmpdir(), "spool-")); roots.push(root); return { root, value: new DurableSpool(root) }; }
describe("DurableSpool", () => {
  test("round trips after reopening and only contiguous completion advances", async () => { const { root, value } = await spool(); expect(await value.append(record)).toBe(1); expect(await value.append({ ...record, revision: "r2" })).toBe(2); await value.complete(2); expect((await value.pending()).map((x) => x.sequence)).toEqual([1, 2]); await value.complete(1); expect(await new DurableSpool(root).pending()).toEqual([]); expect(await readFile(join(root, "spool.durable"), "utf8")).toBeTruthy(); });
  test("rejects quota before writing", async () => { const { root } = await spool(); const limited = new DurableSpool(root, { maxBytes: 10 }); await expect(limited.append(record)).rejects.toThrow("quota"); });
  test("quarantines a corrupt frame", async () => { const { root, value } = await spool(); await value.append(record); const path = join(root, "spool.journal"); const bytes = await readFile(path); const last = bytes.at(-1); if (last === undefined) throw new Error("empty frame"); const corrupted = Buffer.concat([bytes.subarray(0, bytes.length - 1), Buffer.from([last ^ 1])]); await writeFile(path, corrupted); expect((await value.status()).quarantined).toBe(true); expect(await value.pending()).toEqual([]); });
  test("rejects completion for a sequence that was never admitted", async () => { const { value } = await spool(); await expect(value.complete(1)).rejects.toThrow("not pending"); });
  test("recovers only the durable prefix and truncates a torn suffix", async () => { const { root, value } = await spool(); await value.append(record); const durable = Number(await readFile(join(root, "spool.durable"), "utf8")); await writeFile(join(root, "spool.journal"), Buffer.concat([await readFile(join(root, "spool.journal")), Buffer.from([0, 0, 0]) ])); const reopened = new DurableSpool(root); expect((await reopened.pending()).map((entry) => entry.sequence)).toEqual([1]); expect((await readFile(join(root, "spool.journal"))).length).toBe(durable); });
  test("quarantines corruption inside the durable prefix", async () => { const { root, value } = await spool(); await value.append(record); const path = join(root, "spool.journal"); const bytes = await readFile(path); const index = bytes.length - 1; const byte = bytes[index]; if (byte === undefined) throw new Error("empty frame"); bytes[index] = byte ^ 1; await writeFile(path, bytes); const reopened = new DurableSpool(root); expect((await reopened.status()).quarantined).toBe(true); expect(await reopened.pending()).toEqual([]); });
});
