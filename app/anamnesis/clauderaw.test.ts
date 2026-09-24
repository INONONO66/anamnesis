import { expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, readFile, rename, rm, stat, symlink, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { collectClaudeRaw } from "../../packages/backfill/src/clauderaw.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "./client.ts";
import { ingestClaudeRaw } from "./clauderaw.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const hash = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
const saved = async (path: string) => JSON.parse(await readFile(path, "utf8"));
const line = (value: object) => JSON.stringify(value) + "\n";
const turn = (text = "hello", uuid = "turn") => ({ type: "user", uuid, timestamp: "2026-03-01T00:00:00Z", content: text });

/** Helper to build expected RpcRememberParams including lineage metadata for user/assistant roles. */
function expectedParams(input: any): RpcRememberParams {
  const actor = input.origin.actor;
  const metadata = actor === "user" || actor === "assistant" ? { origin_role: actor, lineage_mode: "direct", parent_recall_ids: [] } : {};
  return RpcRememberParams.parse({ episode: { schema: input.schema, content: input.content, time: input.time, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null, ...metadata });
}
function identity(params: RpcRememberParams) {
  const value = { digest_version: 1, params }, keys = new Set<string>();
  const collect = (v: unknown) => { if (v && typeof v === "object") for (const [k, child] of Object.entries(v)) { if (!Array.isArray(v)) keys.add(k); collect(child); } };
  collect(value);
  const o = params.episode.origin;
  return { revision_key: hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision])), body_digest: hash(JSON.stringify(value, [...keys].sort())), data_incarnation: incarnation };
}
async function fixture(run: (source: string, cp: string, path: string) => Promise<void>) {
  const root = await mkdtemp("/tmp/ana-clauderaw-");
  const source = root + "/export", path = source + "/home/.claude/transcripts/session.jsonl";
  await mkdir(source + "/home/.claude/transcripts", { recursive: true });
  await writeFile(path, line({ type: "session", sessionId: "resumed" }) + line(turn()));
  try { await run(source, root + "/checkpoint.json", path); } finally { await rm(root, { recursive: true, force: true }); }
}
function mock(cp: string, loseReply = false) {
  const methods: string[] = [], params: RpcRememberParams[] = [], contexts: unknown[] = [];
  const committed = new Map<string, object>();
  let object: { hash: string; size: number; media_type: string }, chunks: Buffer[] = [];
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: any) => {
    methods.push(method);
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "ingest.status") return committed.get(JSON.stringify(input)) ?? { ...input, state: "unknown" };
    const pending = await saved(cp + ".pending.json");
    expect(pending.identity).toEqual(identity(pending.params));
    if (pending.payload) expect(hash(Buffer.from(pending.payload.bytes_b64, "base64"))).toBe(pending.params.payload_hash);
    if (method === "object.begin") {
      object = { hash: input.sha256, size: input.size, media_type: input.media_type }; chunks = [];
      return { state: "uploading", upload_id: "22222222-2222-4222-8222-222222222222", next_seq: 0 };
    }
    if (method === "object.chunk") { expect(input.seq).toBe(chunks.length); chunks.push(Buffer.from(input.bytes_b64, "base64")); return { next_seq: chunks.length }; }
    if (method === "object.commit") { expect(hash(Buffer.concat(chunks))).toBe(object.hash); expect(Buffer.concat(chunks).length).toBe(object.size); return object; }
    expect(method).toBe("remember"); expect(pending.params).toEqual(input);
    params.push(RpcRememberParams.parse(input)); contexts.push(pending.context);
    const result = { ...identity(input), state: "committed" }; committed.set(JSON.stringify(pending.identity), result);
    if (loseReply) { loseReply = false; throw new Error("lost reply"); }
    return result;
  }});
  return { client, methods, params, contexts };
}

test("preserves parser bodies, nested roots, headers, delegation, compaction and plumbing exclusion", () => fixture(async (source, cp, path) => {
  await writeFile(path, line({ type: "session", sessionId: "resumed" }) + line(turn()) + line({ type: "summary", uuid: "summary", timestamp: 1000, summary: "compacted" }) + line({ type: "assistant", uuid: "mixed", timestamp: 2000, message: { content: [{ type: "tool_use", id: "tool" }, { type: "text", text: "answer" }] } }));
  const delegated = source + "/home/.claude/projects/parent/subagents/workflows/wf_1";
  await mkdir(delegated, { recursive: true });
  await writeFile(delegated + "/agent-child.jsonl", line({ type: "session", sessionId: "parent", cwd: "/project", gitBranch: "main" }) + line({ ...turn("delegated", "child"), isSidechain: true, agentId: "child" }) + line({ type: "system", subtype: "compact_boundary", uuid: "compact", timestamp: 3000, content: "delegate summary" }));
  await writeFile(delegated + "/journal.jsonl", "not a transcript");
  await mkdir(source + "/jobs"); await writeFile(source + "/jobs/timeline.jsonl", "ignored");
  await writeFile(source + "/._sidecar.jsonl", "ignored");
  const expected = await collectClaudeRaw(source), m = mock(cp);
  await ingestClaudeRaw(source, cp, m.client);
  expect(m.params).toHaveLength(expected.length);
  for (const { input } of expected) expect(m.params.find(p => p.episode.origin.record === input.origin.record)).toEqual(expectedParams(input));
  expect(m.params.map(p => p.episode.origin.record)).toEqual(["child", "compact", "turn", "summary", "mixed:content:1"]);
  expect((await saved(cp)).next).toBe(5);
}));

test("long masked payload is durable before upload; lost reply resumes only by matching status", () => fixture(async (source, cp, path) => {
  const raw = "use xoxb-1234567890abcdef " + "x".repeat(600_000);
  await writeFile(path, line({ type: "session", sessionId: "resumed" }) + line(turn(raw)));
  const [expected] = await collectClaudeRaw(source), m = mock(cp, true);
  await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow("lost reply");
  const work = await saved(cp + ".pending.json");
  expect(work.params.source_revision).toBe(hash("2026-03-01T00:00:00.000Z\n" + raw));
  expect(work.params.episode.content).toBe(expected!.input.content);
  expect(Buffer.from(work.payload.bytes_b64, "base64")).toEqual(Buffer.from(expected!.input.payload!));
  expect(work.context).toEqual({ file: "home/.claude/transcripts/session.jsonl", line: 2, native_source_revision: expected!.input.source_revision });
  expect((await saved(cp)).next).toBe(0);
  expect(m.methods).toEqual(["status", "object.begin", "object.chunk", "object.chunk", "object.commit", "remember"]);
  await ingestClaudeRaw(source, cp, m.client);
  expect(m.methods.slice(6)).toEqual(["status", "ingest.status"]);
  expect((await saved(cp)).next).toBe(1); await expect(stat(cp + ".pending.json")).rejects.toHaveProperty("code", "ENOENT");
}));

test("physical order binds duplicate A and observed A-B-A occurrence with exact predecessors", () => fixture(async (source, cp, path) => {
  await writeFile(path, [turn("A"), turn("A"), turn("B"), turn("A")].map(line).join(""));
  const m = mock(cp); await ingestClaudeRaw(source, cp, m.client);
  const [a, duplicate, b, again] = m.params;
  expect(duplicate).toEqual(a); expect(a!.expected_previous_revision_key).toBeNull();
  expect(b!.expected_previous_revision_key).toBe(identity(a!).revision_key);
  expect(again!.expected_previous_revision_key).toBe(identity(b!).revision_key);
  expect(again!.source_revision).toBe(a!.source_revision + ":occurrence:4");
  const before = m.methods.length; await ingestClaudeRaw(source, cp, m.client);
  expect(m.methods.slice(before)).toEqual(["status", "ingest.status"]); expect((await saved(cp)).next).toBe(4);
}));

for (const state of ["unknown", "spooled", "blocked", "quarantined"] as const) test(`${state} never retransmits or advances`, () => fixture(async (source, cp) => {
  const m = mock(cp, true); await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow("lost reply");
  const before = await readFile(cp), pending = await readFile(cp + ".pending.json");
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: object) => {
    if (method === "status") return { data_incarnation: incarnation };
    expect(method).toBe("ingest.status"); return { ...input, state };
  }});
  await expect(ingestClaudeRaw(source, cp, client)).rejects.toHaveProperty("code", `source_pending_${state}`);
  expect(await readFile(cp)).toEqual(before); expect(await readFile(cp + ".pending.json")).toEqual(pending);
}));

for (const change of ["bytes", "permission", "delete", "add", "rotation"] as const) test(`${change} rejects immutable resume`, () => fixture(async (source, cp, path) => {
  const m = mock(cp); await ingestClaudeRaw(source, cp, m.client); const before = await readFile(cp);
  if (change === "bytes") await writeFile(path, line(turn("changed")));
  if (change === "permission") await chmod(path, 0);
  if (change === "delete") await rm(path);
  if (change === "add") await writeFile(source + "/new.jsonl", line(turn("new")));
  if (change === "rotation") { await rename(path, path + ".old"); await writeFile(path, await readFile(path + ".old")); }
  try { await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow(); }
  finally { if (change === "permission") await chmod(path, 0o600); }
  expect(await readFile(cp)).toEqual(before); expect(m.params).toHaveLength(1);
}));

for (const [name, bytes, error] of [
  ["partial", Buffer.from(JSON.stringify(turn())), "source_partial_final_line"],
  ["malformed", Buffer.from("{bad}\n"), "source_invalid_record"],
  ["array", Buffer.from("[]\n"), "source_invalid_record"],
  ["invalid UTF8", Buffer.from([0xc3, 0x28, 0x0a]), "source_invalid_utf8"],
  ["oversized", Buffer.alloc(8 * 1024 * 1024 + 1, 0x61), "source_record_too_large"],
] as const) test(`${name} rejected before RPC even after valid earlier record`, () => fixture(async (source, cp, path) => {
  await writeFile(path, Buffer.concat([Buffer.from(line(turn())), bytes])); const m = mock(cp);
  await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow(error);
  expect(m.methods).toEqual([]); await expect(stat(cp)).rejects.toHaveProperty("code", "ENOENT");
}));

test("mutation during committed reply cannot advance", () => fixture(async (source, cp, path) => {
  const m = mock(cp), request = m.client.request.bind(m.client);
  m.client.request = (async (method: any, input: any) => { const result = await request(method, input); if (method === "remember") await writeFile(path, line(turn("live append"))); return result; }) as typeof m.client.request;
  await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow("source_changed");
  expect((await saved(cp)).next).toBe(0); expect((await saved(cp + ".pending.json")).index).toBe(0);
}));

test("forged committed identity cannot advance", () => fixture(async (source, cp) => {
  const m = mock(cp), request = m.client.request.bind(m.client);
  m.client.request = (async (method: any, input: any) => { const result = await request(method, input); return method === "remember" ? { ...result, body_digest: "f".repeat(64) } : result; }) as typeof m.client.request;
  await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow("source_pending_identity_mismatch");
  expect((await saved(cp)).next).toBe(0);
}));

test("same revision with conflicting delegated context fails complete preflight", () => fixture(async (source, cp) => {
  const directory = source + "/projects/parent/subagents"; await mkdir(directory, { recursive: true });
  await writeFile(directory + "/agent-child.jsonl", line({ ...turn(), cwd: "/one" }) + line({ ...turn(), cwd: "/two" }));
  const m = mock(cp); await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow("source_revision_conflict"); expect(m.methods).toEqual([]);
}));

test("missing, empty, symlinked and checkpoint-inside-export inputs fail explicitly", () => fixture(async (source, cp) => {
  const m = mock(cp);
  await expect(ingestClaudeRaw(source + "/absent", cp, m.client)).rejects.toHaveProperty("code", "ENOENT");
  await mkdir(source + "/empty"); await expect(ingestClaudeRaw(source + "/empty", cp, m.client)).rejects.toThrow("source_no_export_files");
  await symlink(source + "/home", source + "/link"); await expect(ingestClaudeRaw(source, cp, m.client)).rejects.toThrow("source_symlink"); await rm(source + "/link");
  await expect(ingestClaudeRaw(source, source + "/checkpoint.json", m.client)).rejects.toThrow("source_checkpoint_path_conflict"); expect(m.methods).toEqual([]);
}));

test("user and assistant records include lineage metadata (origin_role, lineage_mode, parent_recall_ids)", () => fixture(async (source, cp, path) => {
  await writeFile(path, line({ type: "session", sessionId: "test" }) + line(turn("user msg", "user-turn")) + line({ type: "assistant", uuid: "asst-turn", timestamp: "2026-03-01T00:00:01Z", message: { content: [{ type: "text", text: "assistant msg" }] } }));
  const m = mock(cp);
  await ingestClaudeRaw(source, cp, m.client);
  expect(m.params).toHaveLength(2);
  const userRecord = m.params.find(p => p.episode.origin.record === "user-turn");
  expect(userRecord).toBeDefined();
  expect(userRecord!.origin_role).toBe("user");
  expect(userRecord!.lineage_mode).toBe("direct");
  expect(userRecord!.parent_recall_ids).toEqual([]);
  const assistantRecord = m.params.find(p => p.episode.origin.record === "asst-turn");
  expect(assistantRecord).toBeDefined();
  expect(assistantRecord!.origin_role).toBe("assistant");
  expect(assistantRecord!.lineage_mode).toBe("direct");
  expect(assistantRecord!.parent_recall_ids).toEqual([]);
}));
