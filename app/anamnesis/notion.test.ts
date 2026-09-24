import { expect, test } from "bun:test";
import { chmod, mkdir, mkdtemp, readFile, rename, rm, stat, symlink, utimes, writeFile } from "node:fs/promises";
import { createHash } from "node:crypto";
import { collectNotion } from "../../packages/backfill/src/notion.ts";
import { maskSecrets } from "../../packages/backfill/src/secrets.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { RpcClient } from "./client.ts";
import { ingestNotion } from "./notion.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const hash = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
const saved = async (path: string) => JSON.parse(await readFile(path, "utf8"));
function identity(params: RpcRememberParams) {
  const value = { digest_version: 1, params }, keys = new Set<string>();
  const collect = (v: unknown) => { if (v && typeof v === "object") for (const [k, child] of Object.entries(v)) { if (!Array.isArray(v)) keys.add(k); collect(child); } };
  collect(value);
  const o = params.episode.origin;
  return { revision_key: hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision])), body_digest: hash(JSON.stringify(value, [...keys].sort())), data_incarnation: incarnation };
}
async function fixture(run: (source: string, cp: string) => Promise<void>) {
  const root = await mkdtemp("/tmp/ana-notion-");
  const source = root + "/export";
  await mkdir(source + "/Workspace/nested", { recursive: true });
  await writeFile(source + "/Workspace/nested/Page.md", "# Page\n\nsecret: abcdefghijklmnopqrstuvwxyz\n");
  await utimes(source + "/Workspace/nested/Page.md", new Date(0), new Date("2026-03-01T00:00:00Z"));
  try { await run(source, root + "/checkpoint.json"); } finally { await rm(root, { recursive: true, force: true }); }
}
function mock(cp: string, loseReply = false) {
  const methods: string[] = [], params: RpcRememberParams[] = [], contexts: unknown[] = [];
  const committed = new Map<string, object>();
  let object: { hash: string; size: number; media_type: string }, chunks: Buffer[] = [];
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: any) => {
    methods.push(method);
    if (method === "status") return { data_incarnation: incarnation, storage: "available" };
    if (method === "ingest.status") return committed.get(JSON.stringify(input)) ?? { ...input, state: "unknown" };
    const pending = await saved(cp + ".pending.json");
    expect(pending.identity).toEqual(identity(pending.params));
    expect(hash(Buffer.from(pending.payload.bytes_b64, "base64"))).toBe(pending.params.payload_hash);
    if (method === "object.begin") {
      object = { hash: input.sha256, size: input.size, media_type: input.media_type }; chunks = [];
      return { state: "uploading", upload_id: "22222222-2222-4222-8222-222222222222", next_seq: 0 };
    }
    if (method === "object.chunk") { expect(input.seq).toBe(chunks.length); chunks.push(Buffer.from(input.bytes_b64, "base64")); return { next_seq: chunks.length }; }
    if (method === "object.commit") { expect(hash(Buffer.concat(chunks))).toBe(object.hash); expect(Buffer.concat(chunks).length).toBe(object.size); return object; }
    expect(method).toBe("remember");
    expect(pending.params).toEqual(input);
    params.push(RpcRememberParams.parse(input)); contexts.push(pending.context);
    const result = { ...identity(input), state: "committed" }; committed.set(JSON.stringify(pending.identity), result);
    if (loseReply) { loseReply = false; throw new Error("lost reply"); }
    return result;
  }});
  return { client, methods, params, contexts };
}

test("preserves exact collector page metadata and redaction, commits payload before remember, resumes only through status", () => fixture(async (source, cp) => {
  const [episode] = await collectNotion(source), m = mock(cp, true);
  await expect(ingestNotion(source, cp, m.client)).rejects.toThrow("lost reply");
  const work = await saved(cp + ".pending.json");
  expect(work.params).toEqual(RpcRememberParams.parse({ episode: { schema: episode!.input.schema, content: episode!.input.content, time: episode!.input.time, origin: episode!.input.origin, properties: episode!.input.properties }, source_revision: episode!.input.source_revision, expected_previous_revision_key: null, payload_hash: hash(episode!.input.payload!) }));
  expect(work.context).toEqual({ file: "Workspace/nested/Page.md", line: 1, native_source_revision: episode!.input.source_revision });
  expect(Buffer.from(work.payload.bytes_b64, "base64")).toEqual(Buffer.from(episode!.input.payload!));
  expect(work.payload.media_type).toBe("text/markdown");
  expect((await saved(cp)).next).toBe(0);
  expect(m.methods).toEqual(["status", "object.begin", "object.chunk", "object.commit", "remember"]);
  await ingestNotion(source, cp, m.client);
  expect(m.methods.slice(5)).toEqual(["status", "ingest.status"]);
  expect((await saved(cp)).next).toBe(1);
  await expect(stat(cp + ".pending.json")).rejects.toHaveProperty("code", "ENOENT");
  await ingestNotion(source, cp, m.client);
  expect(m.params).toHaveLength(1);
}));

test("streams file order, not event-time order, with empty pages and distinct path origins (no invented revisions)", () => fixture(async (source, cp) => {
  await writeFile(source + "/Workspace/A.md", "# A\n");
  await utimes(source + "/Workspace/A.md", new Date(0), new Date("2026-09-01T00:00:00Z"));
  await writeFile(source + "/Empty.md", "");
  const m = mock(cp); await ingestNotion(source, cp, m.client);
  expect(m.params.map(p => p.episode.origin.record)).toEqual(["Empty.md", "Workspace/A.md", "Workspace/nested/Page.md"]);
  expect(m.params[0]!.episode.origin.session).toBe("Empty.md");
  expect(m.params[0]!.episode.content).toBe("Empty");
  expect(m.params.every(p => p.expected_previous_revision_key === null)).toBe(true);
  expect((await saved(cp)).next).toBe(3);
}));

test("long redacted Markdown uploads in bounded UDS chunks without exposing secrets", () => fixture(async (source, cp) => {
  const raw = "# Long\nuse xoxb-1234567890abcdef " + "x".repeat(600_000) + "\n";
  await writeFile(source + "/Workspace/nested/Page.md", raw);
  const m = mock(cp, true); await expect(ingestNotion(source, cp, m.client)).rejects.toThrow("lost reply");
  const text = maskSecrets(raw).text, pending = await saved(cp + ".pending.json");
  expect(pending.params.source_revision).toBe(hash(text));
  expect(pending.params.payload_hash).toBe(hash(text));
  expect(Buffer.from(pending.payload.bytes_b64, "base64").toString()).toBe(text);
  expect(pending.params.episode.content).toBe("Page\n\n" + text.replace(/\s+/g, " ").trim().slice(0, 512));
  expect(m.methods.filter(x => x === "object.chunk")).toHaveLength(2);
  await ingestNotion(source, cp, m.client); expect(m.params).toHaveLength(1);
}));

for (const state of ["spooled", "blocked", "quarantined"] as const) test(`${state} never retransmits uploads/remember or advances`, () => fixture(async (source, cp) => {
  const m = mock(cp, true); await expect(ingestNotion(source, cp, m.client)).rejects.toThrow("lost reply");
  const before = await readFile(cp), pending = await readFile(cp + ".pending.json");
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: object) => {
    if (method === "status") return { data_incarnation: incarnation, storage: "available" };
    expect(method).toBe("ingest.status"); return { ...input, state };
  }});
  await expect(ingestNotion(source, cp, client)).rejects.toHaveProperty("code", `source_pending_${state}`);
  expect(await readFile(cp)).toEqual(before); expect(await readFile(cp + ".pending.json")).toEqual(pending);
}));

test("unknown recovery resends only for the same ready incarnation", () => fixture(async (source, cp) => {
  const lost = mock(cp, true); await expect(ingestNotion(source, cp, lost.client)).rejects.toThrow("lost reply");
  const before = await readFile(cp), pending = await readFile(cp + ".pending.json");
  const foreign = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string) => {
    expect(method).toBe("status"); return { data_incarnation: "33333333-3333-4333-8333-333333333333", storage: "available" };
  }});
  await expect(ingestNotion(source, cp, foreign)).rejects.toThrow("incarnation_mismatch");
  expect(await readFile(cp)).toEqual(before); expect(await readFile(cp + ".pending.json")).toEqual(pending);
  // A fresh mock holds no binding for the lost delivery: UNKNOWN from the same
  // ready incarnation retires the pending record and resends it from the checkpoint.
  const m = mock(cp); await ingestNotion(source, cp, m.client);
  expect(m.methods.slice(0, 2)).toEqual(["status", "ingest.status"]); expect(m.methods.filter(x => x === "ingest.status")).toHaveLength(1);
  expect(m.params[0]).toEqual(JSON.parse(pending.toString()).params);
  expect((await saved(cp)).next).toBe(m.params.length); await expect(stat(cp + ".pending.json")).rejects.toHaveProperty("code", "ENOENT");
}));

for (const change of ["bytes", "mtime", "permission", "delete", "add", "rotation"] as const) test(`${change} rejects immutable resume`, () => fixture(async (source, cp) => {
  const m = mock(cp); await ingestNotion(source, cp, m.client); const before = await readFile(cp);
  const path = source + "/Workspace/nested/Page.md";
  if (change === "bytes") await writeFile(path, "changed\n");
  if (change === "mtime") await utimes(path, new Date(0), new Date(0));
  if (change === "permission") await chmod(path, 0);
  if (change === "delete") await rm(path);
  if (change === "add") await writeFile(source + "/new.md", "new\n");
  if (change === "rotation") { await rename(path, path + ".old"); await writeFile(path, await readFile(path + ".old")); }
  try { await expect(ingestNotion(source, cp, m.client)).rejects.toThrow(); }
  finally { if (change === "permission") await chmod(path, 0o600); }
  expect(await readFile(cp)).toEqual(before); expect(m.params).toHaveLength(1);
}));

for (const change of ["append", "delete", "permission", "rotation"] as const) test(`${change} during committed reply leaves pending and does not advance`, () => fixture(async (source, cp) => {
  const m = mock(cp), request = m.client.request.bind(m.client), path = source + "/Workspace/nested/Page.md";
  m.client.request = (async (method: any, input: any) => {
    const result = await request(method, input);
    if (method === "remember") {
      if (change === "append") await writeFile(path, "live append\n");
      if (change === "delete") await rm(path);
      if (change === "permission") await chmod(path, 0);
      if (change === "rotation") { await rename(path, path + ".old"); await writeFile(path, await readFile(path + ".old")); }
    }
    return result;
  }) as typeof m.client.request;
  try { await expect(ingestNotion(source, cp, m.client)).rejects.toThrow(); }
  finally { if (change === "permission") await chmod(path, 0o600); }
  expect((await saved(cp)).next).toBe(0); expect((await saved(cp + ".pending.json")).index).toBe(0);
}));

for (const [name, bytes, error] of [
  ["partial", Buffer.from("# unfinished"), "source_partial_final_line"],
  ["invalid UTF8", Buffer.from([0xc3, 0x28, 0x0a]), "source_invalid_utf8"],
  ["binary", Buffer.from("# bad\0\n"), "source_malformed_markdown"],
  ["oversized", Buffer.alloc(8 * 1024 * 1024 + 1, 0x61), "source_record_too_large"],
] as const) test(`${name} is rejected before any RPC even after a valid earlier page`, () => fixture(async (source, cp) => {
  await writeFile(source + "/Workspace/z.md", bytes); const m = mock(cp);
  await expect(ingestNotion(source, cp, m.client)).rejects.toThrow(error);
  expect(m.methods).toEqual([]); await expect(stat(cp)).rejects.toHaveProperty("code", "ENOENT");
}));

test("missing, empty, symlinked source and checkpoint within export are explicit errors", () => fixture(async (source, cp) => {
  const m = mock(cp);
  await expect(ingestNotion(source + "/absent", cp, m.client)).rejects.toHaveProperty("code", "ENOENT");
  await mkdir(source + "/empty"); await expect(ingestNotion(source + "/empty", cp, m.client)).rejects.toThrow("source_no_export_files");
  await symlink(source + "/Workspace", source + "/link");
  await expect(ingestNotion(source, cp, m.client)).rejects.toThrow("source_symlink");
  await rm(source + "/link");
  await expect(ingestNotion(source, source + "/checkpoint.json", m.client)).rejects.toThrow("source_checkpoint_path_conflict");
  expect(m.methods).toEqual([]);
}));

test("forged committed identity cannot advance", () => fixture(async (source, cp) => {
  const m = mock(cp), request = m.client.request.bind(m.client);
  m.client.request = (async (method: any, input: any) => {
    const result = await request(method, input);
    return method === "remember" ? { ...result, body_digest: "f".repeat(64) } : result;
  }) as typeof m.client.request;
  await expect(ingestNotion(source, cp, m.client)).rejects.toThrow("source_pending_identity_mismatch");
  expect((await saved(cp)).next).toBe(0);
}));
