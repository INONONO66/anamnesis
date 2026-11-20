import { afterEach, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import type { RpcClient } from "./client.ts";
import { loadClean, parseRecord, toRemember, type CleanRecord } from "./load.ts";

const sha256 = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");
const record = (patch: Partial<CleanRecord> = {}): CleanRecord => ({ source: "kakao", room: "room-1", thread: null, message: "m1", speaker: "u1",
  role: "user", time: "2026-01-02T03:04:05.678Z", reply: null, text: "hello", evidence: { speaker_name: "Ino" }, ...patch });
type Batch = { deliveries: { episode: { origin: { record: string; session: string } } }[] };
const roots: string[] = [];
afterEach(async () => { for (const root of roots.splice(0)) await rm(root, { recursive: true, force: true }); });

test("a record becomes a direct-lineage Episode keyed on its event time and raw text, with secrets masked in content", () => {
  const raw = `push with ghp_${"a".repeat(40)}`;
  const built = toRemember(record({ thread: "t9", reply: "kakao:room-1#m0", text: raw }));
  if ("refused" in built) throw new Error(built.refused);
  expect(built.params).toEqual({
    episode: { schema: "anamnesis.original-message/1", time: { value: "2026-01-02T03:04:05.678Z", precision: "second" }, content: "push with [REDACTED]",
      origin: { source: "kakao", session: "room-1/t9", actor: "u1", record: "m1" }, mass: 0.5,
      properties: { speaker_name: "Ino", room: "room-1", thread: "t9", reply: "kakao:room-1#m0" } },
    source_revision: sha256(`2026-01-02T03:04:05.678Z\n${raw}`), expected_previous_revision_key: null,
    origin_role: "user", lineage_mode: "direct", parent_recall_ids: [],
  });
  expect(built.payload).toBeUndefined();
});

test("long text keeps its head without a split surrogate and travels whole as the payload", () => {
  const text = `${"a".repeat(3999)}😀tail`;
  const built = toRemember(record({ text }));
  if ("refused" in built) throw new Error(built.refused);
  expect(built.params.episode.content).toBe("a".repeat(3999));
  expect(built.payload?.toString("utf8")).toBe(text);
  expect(built.params.payload_hash).toBe(sha256(Buffer.from(text, "utf8")));
});

test("blank text is refused and malformed records are rejected", () => {
  expect(toRemember(record({ text: " \n" }))).toEqual({ refused: "empty_text" });
  expect(() => parseRecord(JSON.stringify({ ...record(), role: "narrator" }), "room-1.jsonl:1")).toThrow("clean_record_invalid: room-1.jsonl:1: role");
  expect(() => parseRecord(JSON.stringify({ ...record(), evidence: [] }), "room-1.jsonl:2")).toThrow("evidence");
});

test("a re-run resumes each room after its cursor and sends every record once, in file order", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-load-")); roots.push(root);
  const clean = join(root, "clean"), state = join(root, "state"), corpus = join(clean, "kakao");
  await mkdir(corpus, { recursive: true });
  const line = (room: string, message: string, text: string) => JSON.stringify(record({ room, message, text }));
  await writeFile(join(corpus, "_rooms.jsonl"), [{ file: "a.jsonl", first: "2026-01-01" }, { file: "b.jsonl", first: "2026-01-02" }].map(room => JSON.stringify(room)).join("\n") + "\n");
  await writeFile(join(corpus, "a.jsonl"), [line("a", "1", "first"), line("a", "2", " "), line("a", "3", "third")].join("\n") + "\n");
  await writeFile(join(corpus, "b.jsonl"), [line("b", "1", "fourth"), line("b", "2", "fifth")].join("\n") + "\n");
  const sent: string[][] = [];
  const client = {
    async request(method: string, params: Batch) {
      expect(method).toBe("remember.batch");
      sent.push(params.deliveries.map(delivery => `${delivery.episode.origin.session}#${delivery.episode.origin.record}`));
      return { results: params.deliveries.map(() => ({ state: "committed", created: true })) };
    },
    async close() {},
  } as unknown as RpcClient;
  const options = { clean, source: "kakao", state, concurrency: 1, rooms: [], dryRun: false };
  await loadClean({ ...options, limit: 2 }, async () => client);
  expect(sent).toEqual([["a#1"]]);
  expect(JSON.parse(await readFile(join(state, "kakao", "a.jsonl.cursor.json"), "utf8"))).toMatchObject({ next: 2, created: 1, refused: 1 });
  await loadClean({ ...options, limit: Infinity }, async () => client);
  expect(sent).toEqual([["a#1"], ["a#3"], ["b#1", "b#2"]]);
  const refused = (await readFile(join(state, "kakao", "_refused.jsonl"), "utf8")).trim().split("\n").map(entry => JSON.parse(entry));
  expect(refused).toEqual([{ at: "a.jsonl:2", room: "a", thread: null, message: "2", reason: "empty_text" }]);
});

test("a room goes in batches of at most 32 in file order; a refusal answers its record alone, and a failed batch leaves the cursor before it", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-load-")); roots.push(root);
  const clean = join(root, "clean"), state = join(root, "state"), corpus = join(clean, "kakao");
  await mkdir(corpus, { recursive: true });
  await writeFile(join(corpus, "_rooms.jsonl"), JSON.stringify({ file: "c.jsonl", first: "2026-01-01" }) + "\n");
  await writeFile(join(corpus, "c.jsonl"), Array.from({ length: 70 }, (_, i) => JSON.stringify(record({ room: "c", message: String(i + 1), text: `message ${i + 1}` }))).join("\n") + "\n");
  const sent: string[][] = [];
  let failAt = 3;
  const client = {
    async request(method: string, params: Batch) {
      expect(method).toBe("remember.batch");
      if (sent.length + 1 === failAt) throw Object.assign(new Error("connection lost"), { code: "storage_unavailable", retryable: true });
      sent.push(params.deliveries.map(delivery => delivery.episode.origin.record));
      return { results: params.deliveries.map(delivery => delivery.episode.origin.record === "5"
        ? { state: "refused", code: "revision_conflict", message: "revision already binds a different delivery body" }
        : { state: "committed", created: true }) };
    },
    async close() {},
  } as unknown as RpcClient;
  const options = { clean, source: "kakao", state, concurrency: 1, rooms: [], dryRun: false, limit: Infinity };
  await expect(loadClean(options, async () => client)).rejects.toThrow("connection lost");
  expect(sent.map(batch => batch.length)).toEqual([32, 32]);
  expect(sent.flat()).toEqual(Array.from({ length: 64 }, (_, i) => String(i + 1)));
  const cursorPath = join(state, "kakao", "c.jsonl.cursor.json");
  expect(JSON.parse(await readFile(cursorPath, "utf8"))).toMatchObject({ next: 64, created: 63, refused: 1 });
  const refused = (await readFile(join(state, "kakao", "_refused.jsonl"), "utf8")).trim().split("\n").map(entry => JSON.parse(entry));
  expect(refused).toEqual([{ at: "c.jsonl:5", room: "c", thread: null, message: "5", reason: "revision_conflict" }]);
  failAt = 0;
  await loadClean(options, async () => client);
  expect(sent.slice(2)).toEqual([Array.from({ length: 6 }, (_, i) => String(i + 65))]);
  expect(JSON.parse(await readFile(cursorPath, "utf8"))).toMatchObject({ next: 70, created: 69, refused: 1 });
});
