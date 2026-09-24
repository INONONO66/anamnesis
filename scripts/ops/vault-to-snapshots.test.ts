import { afterAll, beforeAll, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { ingestSource, SOURCE_MAX_BYTES, sourceRevisionKey } from "../../app/anamnesis/source.ts";
import { convertVault, type Manifest } from "./vault-to-snapshots.ts";

const hex = (seed: string) => createHash("sha256").update(seed).digest("hex");
interface Over { source: string; session_id: string; raw: unknown; occurred_at?: string | null; collected_at?: string; role?: unknown; message_id?: string | null; thread_id?: string | null; }
/** One vault file body; `content_hash` is derived from `raw` so identical raw payloads collide like the real vault. */
const vaultRecord = (over: Over) => ({
  schema_version: 1, channel_id: "C-general", message_id: null, thread_id: null, user_id: "U-ino", role: "user", occurred_at: null,
  collected_at: over.occurred_at ?? "2026-07-01T00:00:00.000Z", ts_source: over.occurred_at ? "occurred" : "collected",
  content_hash: `blake3:${hex(JSON.stringify(over.raw))}`, provenance: { machine: "mac", collector: "hub" }, ...over,
});
async function put(root: string, session: string, record: ReturnType<typeof vaultRecord>, body = JSON.stringify(record)): Promise<void> {
  const name = `${record.collected_at.replace(/[:.]/g, "-")}.${String(record.role ?? "unknown")}.${record.content_hash.slice(7, 15)}.json`;
  await mkdir(join(root, session), { recursive: true });
  await writeFile(join(root, session, name), body);
}
const codexMessage = (role: string, type: string, texts: string[]) => ({ type: "response_item", timestamp: "2026-01-01T00:00:00.000Z", payload: { type: "message", role, content: texts.map(text => ({ type, text })) } });
const LONG = "é".repeat(40_000); // 80 KB of two-byte scalars; the 64 KB content cap must land on a scalar boundary.
const EXPECTED: Record<string, string[]> = {
  "sess-new": ["deploy at noon", "deploy at noon (edited)", "gjc says hi", "hello from gjc", "pi text"],
  "sess-mid": ["Read the vault.", "Reading now.", "Vault has 900k records.", "é".repeat(32_768)],
  "sess-old": ["You are a converter.", "How do I shard JSONL?", "Split at session boundaries.\nNever mid-session."],
};
const SKIPPED = { no_text: 4, unsupported_role: 3, invalid: 2, duplicate: 1 };
const FILES = 22;

/** Three sessions, six vault sources, every skip class, one edited message, one exact duplicate, one oversized text. */
async function buildVault(root: string): Promise<void> {
  const old = (over: Omit<Over, "source" | "session_id">) => put(root, "sess-old", vaultRecord({ source: "codex", session_id: "sess-old", ...over }));
  await old({ role: "developer", message_id: "msg-d1", occurred_at: "2026-01-01T00:00:00Z", raw: codexMessage("developer", "input_text", ["You are a converter."]) });
  // Text-bearing tool/system records are skipped as unsupported_role (#215), not emitted without lineage.
  await old({ role: "system", message_id: "msg-s1", occurred_at: "2026-01-01T00:00:00.500Z", raw: codexMessage("system", "input_text", ["System prompt text."]) });
  await old({ role: "tool", message_id: "msg-t1", occurred_at: "2026-01-01T00:00:00.700Z", raw: codexMessage("tool", "output_text", ["tool output text"]) });
  await old({ role: "user", message_id: "msg-u1", occurred_at: "2026-01-01T00:00:01Z", raw: codexMessage("user", "input_text", ["How do I shard JSONL?"]) });
  await old({ role: "assistant", message_id: "msg-a1", occurred_at: "2026-01-01T00:00:02Z", raw: codexMessage("assistant", "output_text", ["Split at session boundaries.", "Never mid-session."]) });
  await old({ role: "assistant", message_id: "msg-f1", occurred_at: "2026-01-01T00:00:03Z", raw: { type: "response_item", timestamp: "t", payload: { type: "function_call", name: "shell", arguments: "{\"cmd\":\"ls\"}" } } });

  const mid = (source: string, over: Omit<Over, "source" | "session_id">) => put(root, "sess-mid", vaultRecord({ source, session_id: "sess-mid", ...over }));
  await mid("claude-code", { role: "user", message_id: "cc-1", occurred_at: "2026-03-01T10:00:00Z", raw: { type: "user", timestamp: "t", content: "Read the vault." } });
  await mid("claude-code", { role: "assistant", message_id: "cc-2", occurred_at: "2026-03-01T10:00:05Z", raw: { type: "assistant", timestamp: "t", content: [{ type: "thinking", thinking: "hmm" }, { type: "text", text: "Reading now." }, { type: "tool_use", id: "t1", name: "Read", input: {} }] } });
  await mid("claude-code", { role: "user", message_id: "cc-3", occurred_at: "2026-03-01T10:00:06Z", raw: { type: "user", timestamp: "t", content: [{ type: "tool_result", tool_use_id: "t1", content: "file body" }] } });
  await mid("claude-code", { role: "assistant", message_id: "cc-4", occurred_at: "2026-03-01T10:00:07Z", raw: { type: "assistant", timestamp: "t", content: [{ type: "text", text: "  \n" }] } });
  const opencode = (type: string, extra: object) => ({ part: "p", part_data: { type, ...extra }, message: "m", message_data: { role: "assistant" }, session_agent: "build", session_model: "qwen", directory: "/work" });
  await mid("opencode", { role: "assistant", message_id: "oc-1", occurred_at: "2026-03-01T10:00:10Z", raw: opencode("text", { text: "Vault has 900k records." }) });
  await mid("opencode", { role: "assistant", message_id: "oc-2", occurred_at: "2026-03-01T10:00:11Z", raw: opencode("tool", { tool: "bash", state: {} }) });
  await mid("opencode", { role: "assistant", message_id: "oc-3", occurred_at: "2026-03-01T10:00:12Z", raw: opencode("text", { text: LONG }) });

  const fresh = (source: string, over: Omit<Over, "source" | "session_id">) => put(root, "sess-new", vaultRecord({ source, session_id: "sess-new", ...over }));
  const slack = (text: string) => ({ text, ts: "1717.0001", type: "message", user: "U-ino" });
  await fresh("slack", { message_id: "1717.0001", occurred_at: "2026-06-01T12:00:00Z", raw: slack("deploy at noon") });
  await fresh("slack", { message_id: "1717.0001", occurred_at: "2026-06-01T12:00:00Z", collected_at: "2026-06-01T12:00:30.000Z", raw: slack("deploy at noon") });
  await fresh("slack", { message_id: "1717.0001", thread_id: "1717.0000", occurred_at: "2026-06-01T12:05:00Z", raw: slack("deploy at noon (edited)") });
  await fresh("gjc", { occurred_at: "2026-06-01T12:10:00Z", raw: { id: "g1", message: "gjc says hi", parentId: null, timestamp: 1, type: "message" } });
  await fresh("gjc", { role: "assistant", message_id: "g2", occurred_at: "2026-06-01T12:11:00Z", raw: { id: "g2", message: { role: "assistant", content: [{ type: "text", text: "hello from gjc" }] }, parentId: "g1", timestamp: 2, type: "message" } });
  await fresh("pi", { role: "assistant", message_id: "p1", occurred_at: "2026-06-01T12:12:00Z", raw: { id: "p1", message: { role: "assistant", content: "pi text" }, parentId: null, timestamp: 3, type: "message" } });
  await fresh("discord", { role: null, message_id: "d1", occurred_at: "2026-06-01T12:13:00Z", raw: { content: "orphan", author: { id: "1" } } });
  await put(root, "sess-new", vaultRecord({ source: "slack", session_id: "sess-new", message_id: "broken", collected_at: "2026-06-01T12:14:00.000Z", raw: slack("broken json") }), "{not json");
  await fresh("slack", { message_id: "1717.0002", occurred_at: "yesterday-ish", collected_at: "2026-06-01T12:15:00.000Z", raw: slack("bad date") });
  await writeFile(join(root, "sess-new", "notes.txt"), "not a record");
  await writeFile(join(root, "sess-new", ".DS_Store"), "");
  await writeFile(join(root, "README.md"), "stray file at the root is not a session");
}

interface Line { file: string; params: RpcRememberParams; bytes: number; }
async function readShards(out: string): Promise<{ files: string[]; lines: Line[] }> {
  const files = (await readdir(out)).filter(name => name.endsWith(".jsonl")).sort();
  const lines: Line[] = [];
  for (const file of files) {
    const text = await readFile(join(out, file), "utf8");
    expect(text.endsWith("\n")).toBe(true);
    for (const line of text.slice(0, -1).split("\n")) lines.push({ file, params: RpcRememberParams.parse(JSON.parse(line)), bytes: Buffer.byteLength(line) + 1 });
  }
  return { files, lines };
}
const sessionsInOrder = (lines: Line[]) => [...new Set(lines.map(l => l.params.episode.origin.session))];
const session = (lines: Line[], id: string) => lines.filter(l => l.params.episode.origin.session === id);

let tmp: string, root: string, outs = 0;
const nextOut = () => join(tmp, `out-${outs++}`);
beforeAll(async () => { tmp = await mkdtemp(join(tmpdir(), "ana-vault-")); root = join(tmp, "vault", "mixed"); await buildVault(root); });
afterAll(async () => { await rm(tmp, { recursive: true, force: true }); });

test("emits bare RpcRememberParams lines: newest session first, ascending within, text extracted per source", async () => {
  const out = nextOut();
  const manifest = await convertVault({ root, out });
  const { files, lines } = await readShards(out);
  expect(files).toEqual(["snapshot-0000.jsonl"]);
  expect(sessionsInOrder(lines)).toEqual(["sess-new", "sess-mid", "sess-old"]);
  for (const [id, contents] of Object.entries(EXPECTED)) {
    const own = session(lines, id);
    expect(own.map(l => l.params.episode.content)).toEqual(contents);
    const times = own.map(l => Date.parse(l.params.episode.time.value));
    expect(times.every((t, i) => i === 0 || t >= times[i - 1]!)).toBe(true);
  }
  expect(new Set(lines.map(l => l.params.episode.origin.source))).toEqual(new Set(["vault-codex", "vault-claude-code", "vault-opencode", "vault-slack", "vault-gjc", "vault-pi"]));
  const first = lines[0]!.params;
  expect(first.episode).toMatchObject({ schema: "anamnesis.original-message/1", mass: 0.5, time: { value: "2026-06-01T12:00:00.000Z", precision: "second" }, origin: { source: "vault-slack", session: "sess-new", actor: "user", record: "1717.0001" } });
  expect(first.episode.properties).toEqual({ vault_channel_id: "C-general", vault_user_id: "U-ino", vault_ts_source: "occurred", vault_content_hash: `blake3:${first.source_revision}` });
  expect(first.source_revision).toMatch(/^[0-9a-f]{64}$/);
  expect(manifest).toMatchObject({ source: "mixed", sessions: 3, files: FILES, records: 12, skipped: SKIPPED, split_sessions: [] });
  expect(JSON.parse(await readFile(join(out, "manifest.json"), "utf8"))).toEqual(manifest);
});

test("revision chain follows the origin head: edited message links to its previous revision, everything else starts at null", async () => {
  const out = nextOut();
  await convertVault({ root, out });
  const { lines } = await readShards(out);
  const [rev1, rev2] = session(lines, "sess-new").filter(l => l.params.episode.origin.record === "1717.0001").map(l => l.params);
  expect(rev1!.episode.content).toBe("deploy at noon");
  expect(rev1!.expected_previous_revision_key).toBeNull();
  expect(rev2!.episode.content).toBe("deploy at noon (edited)");
  expect(rev2!.source_revision).not.toBe(rev1!.source_revision);
  expect(rev2!.expected_previous_revision_key).toBe(sourceRevisionKey(rev1!));
  expect(rev2!.episode.properties["vault_thread_id"]).toBe("1717.0000");
  for (const { params } of lines) if (params !== rev2) expect(params.expected_previous_revision_key).toBeNull();
  // message_id null -> the record identity is the content hash itself.
  const gjc = lines.find(l => l.params.episode.content === "gjc says hi")!.params;
  expect(gjc.episode.origin.record).toBe(gjc.source_revision);
  expect(gjc.episode.properties["vault_content_hash"]).toBe(`blake3:${gjc.source_revision}`);
});

test("lineage metadata only for user/assistant; oversized content is cut on a UTF-8 boundary and flagged", async () => {
  const out = nextOut();
  await convertVault({ root, out });
  const { lines } = await readShards(out);
  for (const { params } of lines) {
    const actor = params.episode.origin.actor;
    if (actor === "user" || actor === "assistant") expect(params).toMatchObject({ origin_role: actor, lineage_mode: "direct", parent_recall_ids: [] });
    else { expect(actor).toBe("developer"); for (const key of ["origin_role", "lineage_mode", "parent_recall_ids"]) expect(key in params).toBe(false); }
  }
  expect(lines.filter(l => l.params.episode.origin.actor === "developer")).toHaveLength(1);
  const long = lines.find(l => l.params.episode.origin.record === "oc-3")!.params;
  expect(Buffer.byteLength(long.episode.content)).toBe(64 * 1024);
  expect(long.episode.content).toBe("é".repeat(32_768));
  expect(long.episode.properties["vault_truncated"]).toBe(true);
  expect(lines.filter(l => "vault_truncated" in l.params.episode.properties)).toHaveLength(1);
});

test("shard byte limit is respected and whole sessions move between shards", async () => {
  const single = nextOut();
  await convertVault({ root, out: single });
  const reference = await readShards(single);
  const perSession = new Map<string, number>();
  for (const line of reference.lines) perSession.set(line.params.episode.origin.session, (perSession.get(line.params.episode.origin.session) ?? 0) + line.bytes);
  const limit = Math.max(...perSession.values());
  const out = nextOut();
  const manifest = await convertVault({ root, out, shardBytes: limit });
  const { files, lines } = await readShards(out);
  expect(files.length).toBeGreaterThanOrEqual(2);
  expect(manifest.shards.map(s => s.file)).toEqual(files);
  for (const shard of manifest.shards) {
    expect(shard.bytes).toBeLessThanOrEqual(limit);
    expect((await stat(join(out, shard.file))).size).toBe(shard.bytes);
    const own = lines.filter(l => l.file === shard.file);
    expect(own).toHaveLength(shard.records);
    expect(new Set(own.map(l => l.params.episode.origin.session)).size).toBe(shard.sessions);
    expect(shard.newest_time).toBe(new Date(Math.max(...own.map(l => Date.parse(l.params.episode.time.value)))).toISOString());
    expect(shard.oldest_time).toBe(new Date(Math.min(...own.map(l => Date.parse(l.params.episode.time.value)))).toISOString());
  }
  for (const id of Object.keys(EXPECTED)) expect(new Set(session(lines, id).map(l => l.file)).size).toBe(1);
  expect(manifest.split_sessions).toEqual([]);
  expect(manifest.shards.reduce((sum, s) => sum + s.records, 0)).toBe(lines.length);
  expect(lines.map(l => l.params)).toEqual(reference.lines.map(l => l.params));
});

test("a session larger than the limit spans consecutive shards in order and is reported", async () => {
  const single = nextOut();
  await convertVault({ root, out: single });
  const reference = await readShards(single);
  // Exactly one line wide: the session holding the largest line cannot fit whole, yet every line still fits a shard.
  const limit = Math.max(...reference.lines.map(l => l.bytes));
  const out = nextOut();
  const manifest = await convertVault({ root, out, shardBytes: limit });
  const { files, lines } = await readShards(out);
  expect(lines.map(l => l.params)).toEqual(reference.lines.map(l => l.params));
  for (const shard of manifest.shards) expect(shard.bytes).toBeLessThanOrEqual(limit);
  const spanning = Object.keys(EXPECTED).filter(id => session(reference.lines, id).reduce((sum, l) => sum + l.bytes, 0) > limit);
  expect(spanning.length).toBeGreaterThan(0);
  expect(manifest.split_sessions).toEqual(sessionsInOrder(lines).filter(id => spanning.includes(id)));
  for (const id of Object.keys(EXPECTED)) {
    const own = [...new Set(session(lines, id).map(l => files.indexOf(l.file)))];
    expect(own).toEqual(own.map((_, i) => own[0]! + i)); // consecutive shard indices
  }
  const [rev1, rev2] = session(lines, "sess-new").filter(l => l.params.episode.origin.record === "1717.0001");
  expect(files.indexOf(rev1!.file)).toBeLessThanOrEqual(files.indexOf(rev2!.file));
});

test("--since and --limit-sessions select whole sessions, newest first", async () => {
  const since = nextOut();
  const sinceManifest = await convertVault({ root, out: since, since: "2026-02-01T00:00:00Z" });
  expect(sessionsInOrder((await readShards(since)).lines)).toEqual(["sess-new", "sess-mid"]);
  expect(sinceManifest).toMatchObject({ sessions: 2, files: 16, records: 9, skipped: { no_text: 3, unsupported_role: 1, invalid: 2, duplicate: 1 } });
  const limited = nextOut();
  const limitedManifest = await convertVault({ root, out: limited, limitSessions: 1 });
  expect(sessionsInOrder((await readShards(limited)).lines)).toEqual(["sess-new"]);
  expect(limitedManifest).toMatchObject({ sessions: 1, files: 9, records: 5 });
  const none = nextOut();
  const noneManifest = await convertVault({ root, out: none, limitSessions: 0 });
  expect(noneManifest).toMatchObject({ sessions: 0, files: 0, records: 0, shards: [] });
  expect(await readdir(none)).toEqual(["manifest.json"]);
});

test("rejects unknown vault sources, bad options, and a non-empty output directory", async () => {
  const other = join(tmp, "vault", "notion");
  await put(other, "n-1", vaultRecord({ source: "notion", session_id: "n-1", occurred_at: "2026-05-01T00:00:00Z", raw: { text: "page" } }));
  await expect(convertVault({ root: other, out: nextOut() })).rejects.toThrow(/vault_source_unsupported: notion/);
  await expect(convertVault({ root, out: nextOut(), since: "not a date" })).rejects.toThrow(/vault_since_invalid/);
  await expect(convertVault({ root, out: nextOut(), shardBytes: SOURCE_MAX_BYTES + 1 })).rejects.toThrow(/vault_shard_bytes_invalid/);
  const used = nextOut();
  await convertVault({ root, out: used });
  await expect(convertVault({ root, out: used })).rejects.toThrow(/vault_out_not_empty/);
});

test("a produced shard is consumed end-to-end by ingestSource (the `ops ingest` adapter)", async () => {
  const out = nextOut();
  await convertVault({ root, out });
  const { lines } = await readShards(out);
  const shard = join(out, "snapshot-0000.jsonl"), checkpoint = join(tmp, `checkpoint-${outs}.json`);
  const committed = new Map<string, any>(), remembered: RpcRememberParams[] = [];
  const client = Object.assign(Object.create(RpcClient.prototype), { request: async (method: string, p: any) => {
    if (method === "status") return { data_incarnation: "11111111-1111-4111-8111-111111111111" };
    if (method === "ingest.status") return committed.get(p.revision_key) ?? { ...p, state: "unknown" };
    if (method === "remember") {
      const pending = JSON.parse(await readFile(checkpoint + ".pending.json", "utf8"));
      remembered.push(p);
      const r = { ...pending.identity, state: "committed", created: true, id: "id", ingest_seq: remembered.length };
      committed.set(p.revision_key, r);
      return r;
    }
    throw new Error(method);
  } });
  await ingestSource(shard, checkpoint, client as RpcClient);
  expect(remembered).toEqual(lines.map(l => l.params));
  expect(JSON.parse(await readFile(checkpoint, "utf8")).next).toBe(lines.length);
});

test("CLI writes the shards and prints exactly one JSON summary line", async () => {
  const out = nextOut();
  const cwd = join(import.meta.dir, "..", "..");
  const proc = Bun.spawn(["bun", "scripts/ops/vault-to-snapshots.ts", "--root", root, "--out", out, "--limit-sessions", "2"], { cwd, stdout: "pipe", stderr: "pipe" });
  const [code, stdout, stderr] = await Promise.all([proc.exited, new Response(proc.stdout).text(), new Response(proc.stderr).text()]);
  expect(code).toBe(0);
  const summaryLines = stdout.trim().split("\n");
  expect(summaryLines).toHaveLength(1);
  const summary = JSON.parse(summaryLines[0]!);
  expect(summary).toMatchObject({ event: "vault_snapshots", source: "mixed", sessions: 2, records: 9, shards: 1, manifest: join(out, "manifest.json") });
  expect(summary.bytes).toBe((await stat(join(out, "snapshot-0000.jsonl"))).size);
  expect(JSON.parse(stderr.trim().split("\n")[0]!)).toMatchObject({ event: "shard_written", file: "snapshot-0000.jsonl" });
  const manifest: Manifest = JSON.parse(await readFile(join(out, "manifest.json"), "utf8"));
  expect(manifest.shards[0]!.records).toBe(9);
  const usage = Bun.spawn(["bun", "scripts/ops/vault-to-snapshots.ts"], { cwd, stdout: "pipe", stderr: "pipe" });
  const [usageCode, usageErr] = await Promise.all([usage.exited, new Response(usage.stderr).text()]);
  expect(usageCode).toBe(2);
  expect(usageErr).toContain("usage:");
});
