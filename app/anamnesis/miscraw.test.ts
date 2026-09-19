import { expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, readFile, rename, rm, stat, symlink, utimes, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { collectMiscRaw } from "../../packages/backfill/src/miscraw.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "./client.ts";
import { ingestMiscRaw } from "./miscraw.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const hash = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
const saved = async (path: string) => JSON.parse(await readFile(path, "utf8"));
const line = (value: object) => JSON.stringify(value) + "\n";
const aside = "aside/home/.aside/u/0/sessions/2026-06-30_native/messages.jsonl";
const index = "aside/home/.aside/u/0/sessions.jsonl";
const ag = "gemini-antigravity/home/.gemini/antigravity-cli/brain/brain/.system_generated/logs/transcript.jsonl";
const oc = "opencode/home/.local/state/opencode/prompt-history.jsonl";
const turn = (text = "hello") => ({ role: "user", timestamp: 1782794590501, content: text });
const step = (text = "A") => ({ type: "USER_INPUT", step_index: 42, created_at: "2026-06-14T07:35:41Z", content: `<USER_REQUEST>\n${text}\n</USER_REQUEST>\nmetadata` });
async function put(root: string, name: string, bytes: string | Buffer) { await mkdir(root + "/" + name.slice(0, name.lastIndexOf("/")), { recursive: true }); await writeFile(root + "/" + name, bytes); }
async function seal(root: string, names = [aside, index]) {
  await writeFile(root + "/miscraw.snapshot.json", line({ format: "misc-raw-snapshot/1", sealed: true, files: await Promise.all(names.map(async path => ({ path, sha256: hash(await readFile(root + "/" + path)), ...(path === oc ? { mtime_ms: (await stat(root + "/" + path)).mtime.getTime() } : {}) }))) }));
}
function identity(params: RpcRememberParams) {
  const value = { digest_version: 1, params }, keys = new Set<string>();
  const collect = (v: unknown) => { if (v && typeof v === "object") for (const [k, child] of Object.entries(v)) { if (!Array.isArray(v)) keys.add(k); collect(child); } };
  collect(value);
  const o = params.episode.origin;
  return { revision_key: hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision])), body_digest: hash(JSON.stringify(value, [...keys].sort())), data_incarnation: incarnation };
}
async function fixture(run: (source: string, cp: string) => Promise<void>) {
  const root = await mkdtemp("/tmp/ana-miscraw-"), source = root + "/export";
  await put(source, aside, line(turn())); await put(source, index, ""); await seal(source);
  try { await run(source, root + "/checkpoint.json"); } finally { await rm(root, { recursive: true, force: true }); }
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
    if (method === "object.begin") { object = { hash: input.sha256, size: input.size, media_type: input.media_type }; chunks = []; return { state: "uploading", upload_id: "22222222-2222-4222-8222-222222222222", next_seq: 0 }; }
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

test("all supported stores retain backfill bodies, native ordinals, tool exclusion and source metadata", () => fixture(async (source, cp) => {
  await put(source, aside, line({ role: "system-message", content: "harness" }) + "\n" + line(turn()) + line({ role: "assistant", timestamp: 1782794591501, content: [{ type: "text", text: "answer" }, { type: "thinking", text: "hidden" }, { type: "toolCall", text: "hidden" }] }));
  await put(source, ag, line(step()) + line({ type: "RUN_COMMAND", content: "tool" }) + line({ ...step("answer"), type: "PLANNER_RESPONSE", step_index: 43 }));
  await put(source, oc, line({ input: "paste", parts: [{ type: "text", text: "body" }, { type: "file", text: "hidden" }], mode: "normal" }));
  await utimes(source + "/" + oc, new Date(1784000000000), new Date(1784000000000));
  await seal(source, [aside, index, ag, oc]);
  const expected = await collectMiscRaw(source), m = mock(cp); await ingestMiscRaw(source, cp, m.client);
  expect(m.params).toHaveLength(expected.length);
  for (const { input } of expected) expect(m.params.find(p => p.episode.origin.source === input.origin.source && p.episode.origin.record === input.origin.record)).toEqual(RpcRememberParams.parse({ episode: { schema: input.schema, content: input.content, time: input.time, origin: input.origin, properties: input.properties }, source_revision: input.source_revision, expected_previous_revision_key: null }));
  expect(m.params.map(p => p.episode.origin.record)).toEqual(["native:1", "native:2", "brain:42", "brain:43", "prompt-history:0"]);
  expect((await saved(cp)).next).toBe(5);
}));
test("Aside normalized index joins by user and session suffix, never session_runs duplicates", () => fixture(async (source, cp) => {
  await put(source, index, line({ id: "native", title: "title", cwd: "/work" })); await seal(source);
  const m = mock(cp); await ingestMiscRaw(source, cp, m.client);
  expect(m.params[0]!.episode.properties).toEqual({ kind: "message", session_title: "title", cwd: "/work" });
}));
test("long masked pending body/hash/context precede object RPC; lost reply only reconciles matching status", () => fixture(async (source, cp) => {
  const raw = "use xoxb-1234567890abcdef " + "x".repeat(600_000);
  await put(source, aside, line(turn(raw))); await seal(source);
  const [expected] = await collectMiscRaw(source), m = mock(cp, true);
  await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("lost reply");
  const work = await saved(cp + ".pending.json");
  expect(work.params.source_revision).toBe(hash("2026-06-30T04:43:10.501Z\n" + raw));
  expect(work.params.episode.content).toBe(expected!.input.content);
  expect(Buffer.from(work.payload.bytes_b64, "base64")).toEqual(Buffer.from(expected!.input.payload!));
  expect(work.context).toEqual({ file: aside, line: 1, native_source_revision: expected!.input.source_revision });
  expect((await saved(cp)).next).toBe(0);
  expect(m.methods).toEqual(["status", "object.begin", "object.chunk", "object.chunk", "object.commit", "remember"]);
  await ingestMiscRaw(source, cp, m.client);
  expect(m.methods.slice(6)).toEqual(["status", "ingest.status"]);
  expect((await saved(cp)).next).toBe(1); await expect(stat(cp + ".pending.json")).rejects.toHaveProperty("code", "ENOENT");
}));
test("Antigravity duplicate and observed A-B-A retain native ID and monotonic occurrence predecessors", () => fixture(async (source, cp) => {
  await put(source, ag, [step(), step(), step("B"), step()].map(line).join("")); await seal(source, [aside, index, ag]);
  const m = mock(cp); await ingestMiscRaw(source, cp, m.client);
  const [, a, duplicate, b, again] = m.params;
  expect(duplicate).toEqual(a); expect(a!.expected_previous_revision_key).toBeNull();
  expect(b!.expected_previous_revision_key).toBe(identity(a!).revision_key);
  expect(again!.expected_previous_revision_key).toBe(identity(b!).revision_key);
  expect(again!.source_revision).toBe(a!.source_revision + ":occurrence:5");
  const before = m.methods.length; await ingestMiscRaw(source, cp, m.client);
  expect(m.methods.slice(before)).toEqual(["status", "ingest.status"]); expect((await saved(cp)).next).toBe(5);
}));
for (const state of ["unknown", "spooled", "blocked", "quarantined"] as const) test(`${state} never retransmits or advances`, () => fixture(async (source, cp) => {
  const m = mock(cp, true); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("lost reply");
  const before = await readFile(cp), pending = await readFile(cp + ".pending.json");
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: object) => {
    if (method === "status") return { data_incarnation: incarnation };
    expect(method).toBe("ingest.status"); return { ...input, state };
  }});
  await expect(ingestMiscRaw(source, cp, client)).rejects.toHaveProperty("code", `source_pending_${state}`);
  expect(await readFile(cp)).toEqual(before); expect(await readFile(cp + ".pending.json")).toEqual(pending);
}));
for (const change of ["bytes", "permission", "delete", "add", "rotation", "index"] as const) test(`${change} rejects immutable resume`, () => fixture(async (source, cp) => {
  const m = mock(cp); await ingestMiscRaw(source, cp, m.client); const before = await readFile(cp), path = source + "/" + aside;
  if (change === "bytes") await writeFile(path, line(turn("changed")));
  if (change === "permission") await chmod(path, 0);
  if (change === "delete") await rm(path);
  if (change === "add") await put(source, ag, line(step()));
  if (change === "rotation") { await rename(path, path + ".old"); await writeFile(path, await readFile(path + ".old")); await rm(path + ".old"); }
  if (change === "index") await put(source, index, line({ id: "native", title: "changed" }));
  try { await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow(); }
  finally { if (change === "permission") await chmod(path, 0o600); }
  expect(await readFile(cp)).toEqual(before); expect(m.params).toHaveLength(1);
}));
for (const [name, bytes, error] of [
  ["partial", Buffer.from(JSON.stringify(turn())), "source_partial_final_line"],
  ["malformed", Buffer.from("{bad}\n"), "source_invalid_record"],
  ["array", Buffer.from("[]\n"), "source_invalid_record"],
  ["timestamp", Buffer.from(line({ ...turn(), timestamp: "bad" })), "source_invalid_record"],
  ["invalid UTF8", Buffer.from([0xc3, 0x28, 0x0a]), "source_invalid_utf8"],
  ["oversized", Buffer.alloc(8 * 1024 * 1024 + 1, 0x61), "source_record_too_large"],
] as const) test(`${name} rejected before RPC even after valid earlier record`, () => fixture(async (source, cp) => {
  await put(source, aside, Buffer.concat([Buffer.from(line(turn())), bytes])); await seal(source); const m = mock(cp);
  await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow(error);
  expect(m.methods).toEqual([]); await expect(stat(cp)).rejects.toHaveProperty("code", "ENOENT");
}));
test("mutation after committed response cannot advance checkpoint", () => fixture(async (source, cp) => {
  const m = mock(cp), request = m.client.request.bind(m.client);
  m.client.request = (async (method: any, input: any) => { const result = await request(method, input); if (method === "remember") await put(source, aside, line(turn("live append"))); return result; }) as typeof m.client.request;
  await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_changed");
  expect((await saved(cp)).next).toBe(0); expect((await saved(cp + ".pending.json")).index).toBe(0);
}));
test("forged committed identity cannot advance", () => fixture(async (source, cp) => {
  const m = mock(cp), request = m.client.request.bind(m.client);
  m.client.request = (async (method: any, input: any) => { const result = await request(method, input); return method === "remember" ? { ...result, body_digest: "f".repeat(64) } : result; }) as typeof m.client.request;
  await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_pending_identity_mismatch"); expect((await saved(cp)).next).toBe(0);
}));
for (const name of ["state.db", "state.db-wal", "state.db-shm", "opencode.sqlite", "conversations.db"]) test(`${name} rejected without touching SQLite bytes`, () => fixture(async (source, cp) => {
  const path = source + "/aside/home/.aside/u/0/" + name;
  const bytes = Buffer.from("SQLite format 3\0fixture"); await writeFile(path, bytes); const before = await stat(path), m = mock(cp);
  await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_sqlite_unsupported");
  expect(await readFile(path)).toEqual(bytes); const after = await stat(path); expect(after.mtimeMs).toBe(before.mtimeMs); expect(after.size).toBe(before.size); expect(m.methods).toEqual([]);
}));
test("missing producer seal, index, unknown format and symlink are explicit preflight errors", () => fixture(async (source, cp) => {
  const m = mock(cp);
  await rm(source + "/miscraw.snapshot.json"); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_producer_seal_required"); await seal(source);
  await rm(source + "/" + index); await seal(source, [aside]); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_aside_index_required");
  await put(source, index, ""); await seal(source);
  await put(source, "normalized/events.jsonl", line({ role: "user", text: "not guessed" })); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_format_unsupported"); await rm(source + "/normalized", { recursive: true });
  await symlink(source + "/aside", source + "/link"); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_symlink"); await rm(source + "/link");
  await expect(ingestMiscRaw(source, source + "/checkpoint.json", m.client)).rejects.toThrow("source_checkpoint_path_conflict"); expect(m.methods).toEqual([]);
}));
test("seal digest and OpenCode producer mtime are required, not inferred", () => fixture(async (source, cp) => {
  await put(source, oc, line({ input: "prompt" })); await seal(source, [aside, index, oc]);
  const data = await saved(source + "/miscraw.snapshot.json"); delete data.files[2].mtime_ms; await writeFile(source + "/miscraw.snapshot.json", line(data));
  const m = mock(cp); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_mtime_mismatch");
  await seal(source, [aside, index, oc]); await put(source, aside, line(turn("tampered"))); await expect(ingestMiscRaw(source, cp, m.client)).rejects.toThrow("source_seal_mismatch"); expect(m.methods).toEqual([]);
}));
