import { expect, test } from "bun:test";
import { chmod, mkdtemp, writeFile, rm, readFile, stat } from "node:fs/promises";
import { maskSecrets } from "../../packages/backfill/src/secrets.ts";
import { createHash } from "node:crypto";
import { ingestAgentLog } from "./agentlog.ts";
import { RpcClient } from "./client.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const hash = (value: string | Uint8Array) => createHash("sha256").update(value).digest("hex");
const event = (text = "A", id = "id", occurred_at = 5000) => ({ provider: "codex", partition_id: "s", upstream_event_id: id, occurred_at, role: "user", canonical_kind: "agent_message", kind: "message", text });
async function fixture(run: (root: string, cp: string) => Promise<void>, events = [event(), event(), event("B"), event()]) {
  const root = await mkdtemp("/tmp/ana-agentlog-");
  await writeFile(root + "/codex.jsonl", events.map(e => JSON.stringify(e)).join("\n") + "\n");
  try { await run(root, root + "/checkpoint.json"); } finally { await rm(root, { recursive: true, force: true }); }
}
const saved = async (path: string) => JSON.parse(await readFile(path, "utf8"));
// Independent canonicalizer and identity, not copied from the pending receipt.
function identity(params: RpcRememberParams) {
  const value = { digest_version: 1, params }, keys = new Set<string>();
  const collect = (v: unknown) => { if (v && typeof v === "object") for (const [k, child] of Object.entries(v)) { if (!Array.isArray(v)) keys.add(k); collect(child); } };
  collect(value);
  const o = params.episode.origin;
  return { revision_key: hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision])), body_digest: hash(JSON.stringify(value, [...keys].sort())), data_incarnation: incarnation };
}
function mock(cp: string, loseReply = false) {
  const calls: string[] = [], params: RpcRememberParams[] = [], committed = new Map<string, unknown>();
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: unknown) => {
    calls.push(method);
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "ingest.status") return committed.get(JSON.stringify(input)) ?? { ...input as object, state: "unknown" };
    if (method === "remember") {
      const pending = await saved(cp + ".pending.json");
      expect(pending.params).toEqual(input);
      const parsed = RpcRememberParams.parse(input);
      params.push(parsed);
      expect(pending.identity).toEqual(identity(parsed));
      const result = { state: "committed", ...identity(parsed) };
      committed.set(JSON.stringify(pending.identity), result);
      if (loseReply) { loseReply = false; throw new Error("lost reply"); }
      return result;
    }
    throw new Error(`unexpected ${method}`);
  }});
  return { client, calls, params };
}

test("normalized snapshot preserves native revision, duplicate predecessor, and observed A-B-A occurrence across lost reply resume", () => fixture(async (root, cp) => {
  const m = mock(cp, true);
  await expect(ingestAgentLog(root, cp, m.client)).rejects.toThrow("lost reply");
  expect((await saved(cp)).next).toBe(0);
  const pending = await saved(cp + ".pending.json");
  expect(pending.context).toEqual({ file: "codex.jsonl", line: 1, native_source_revision: hash("1970-01-01T00:00:05.000Z\nA") });
  await ingestAgentLog(root, cp, m.client);
  expect(m.params).toHaveLength(4); // first delivery reconciled, not sent twice
  const [a, duplicate, b, returned] = m.params;
  expect(a!.source_revision).toBe(hash("1970-01-01T00:00:05.000Z\nA"));
  expect(duplicate).toEqual(a);
  expect(b!.expected_previous_revision_key).toBe(pending.identity.revision_key);
  expect(returned!.source_revision).not.toBe(a!.source_revision);
  expect(returned!.episode).toEqual(a!.episode);
  expect(returned!.expected_previous_revision_key).not.toBeNull();
  expect((await saved(cp)).next).toBe(4);
  await expect(stat(cp + ".pending.json")).rejects.toHaveProperty("code", "ENOENT");
  const sends = m.params.length;
  await ingestAgentLog(root, cp, m.client);
  expect(m.params).toHaveLength(sends);
}));

test("changed immutable snapshot cannot enter an existing checkpoint", () => fixture(async (root, cp) => {
  const m = mock(cp);
  await ingestAgentLog(root, cp, m.client);
  await writeFile(root + "/codex.jsonl", JSON.stringify(event("changed")) + "\n");
  await expect(ingestAgentLog(root, cp, m.client)).rejects.toThrow("source_changed");
  expect(m.params).toHaveLength(4);
}));

for (const [name, content, error] of [
  ["partial", JSON.stringify(event()), "source_partial_final_line"],
  ["malformed", "{bad}\n", "source_parse_error"],
  ["oversized", "x".repeat(1024 * 1024 + 1), "source_record_too_large"],
] as const) test(`${name} snapshot is not zero-record success`, () => fixture(async (root, cp) => {
  await writeFile(root + "/codex.jsonl", content);
  const m = mock(cp);
  await expect(ingestAgentLog(root, cp, m.client)).rejects.toThrow(error);
  expect(m.params).toHaveLength(0);
}));

test("missing source is an explicit error", () => fixture(async (root, cp) => {
  await expect(ingestAgentLog(root + "/missing", cp, mock(cp).client)).rejects.toHaveProperty("code", "ENOENT");
}));


test("long redacted payload is durably bound before object chunks and lost remember reply", () => fixture(async (root, cp) => {
  const raw = "token xoxb-1234567890abcdef " + "x".repeat(600_000);
  const masked = maskSecrets(raw).text;
  await writeFile(root + "/codex.jsonl", JSON.stringify(event(raw)) + "\n");
  const m = mock(cp, true), request = m.client.request.bind(m.client);
  const chunks: Buffer[] = [], methods: string[] = [];
  const uploadId = "22222222-2222-4222-8222-222222222222";
  const object = { hash: hash(masked), size: Buffer.byteLength(masked), media_type: "text/plain" };
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: any) => {
    methods.push(method);
    if (method.startsWith("object.")) {
      const pending = await saved(cp + ".pending.json");
      expect(Buffer.from(pending.payload.bytes_b64, "base64").toString()).toBe(masked);
      expect(pending.params.payload_hash).toBe(hash(masked));
      expect(pending.params.episode.mass).toBe(0.5);
      expect(pending.params.episode.origin).toEqual({ source: "codex", session: "s", actor: "user", record: "id" });
      if (method === "object.begin") { expect(input).toEqual({ sha256: object.hash, size: object.size, media_type: object.media_type }); return { state: "uploading", upload_id: uploadId, next_seq: 0, chunk_bytes_max: 512 * 1024 }; }
      if (method === "object.chunk") { expect(input.seq).toBe(chunks.length); chunks.push(Buffer.from(input.bytes_b64, "base64")); return { upload_id: uploadId, next_seq: chunks.length }; }
      expect(Buffer.concat(chunks).toString()).toBe(masked); return object;
    }
    return request(method as Parameters<typeof request>[0], input);
  }});
  await expect(ingestAgentLog(root, cp, client)).rejects.toThrow("lost reply");
  expect(methods).toEqual(["status", "object.begin", "object.chunk", "object.chunk", "object.commit", "remember"]);
  await ingestAgentLog(root, cp, client);
  expect(methods.slice(6)).toEqual(["status", "ingest.status"]);
  expect(m.params).toHaveLength(1);
  expect(m.params[0]!.source_revision).toBe(hash("1970-01-01T00:00:05.000Z\n" + raw));
  expect(m.params[0]!.episode.content).toBe(masked.slice(0, 4000));
}));

test("physical export order is retained while exact non-recallable line context is replayed", () => fixture(async (root, cp) => {
  await writeFile(root + "/codex.jsonl", [event("later", "later", 9000), event(" "), event("earlier", "earlier", 1000)].map(e => JSON.stringify(e)).join("\n") + "\n");
  const m = mock(cp);
  await ingestAgentLog(root, cp, m.client);
  expect(m.params.map(p => p.episode.content)).toEqual(["later", "earlier"]);
  expect((await saved(cp)).next).toBe(2);
}));

test("mutation after committed reply cannot publish a successful checkpoint", () => fixture(async (root, cp) => {
  const m = mock(cp), request = m.client.request.bind(m.client);
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: Parameters<typeof request>[0], input: any) => {
    const result = await request(method, input);
    if (method === "remember") await writeFile(root + "/codex.jsonl", JSON.stringify(event("changed")) + "\n");
    return result;
  }});
  await expect(ingestAgentLog(root, cp, client)).rejects.toThrow("source_changed");
  expect((await saved(cp)).next).toBe(0);
  expect((await saved(cp + ".pending.json")).index).toBe(0);
}));

for (const change of ["delete", "permission", "add", "rename"] as const) test(`${change} is not successful immutable resume`, () => fixture(async (root, cp) => {
  const m = mock(cp);
  await ingestAgentLog(root, cp, m.client);
  if (change === "delete") await rm(root + "/codex.jsonl");
  if (change === "permission") await chmod(root + "/codex.jsonl", 0);
  if (change === "add" || change === "rename") await writeFile(root + "/new.jsonl", JSON.stringify(event("new")) + "\n");
  if (change === "rename") await rm(root + "/codex.jsonl");
  try { await expect(ingestAgentLog(root, cp, m.client)).rejects.toThrow(); }
  finally { if (change === "permission") await chmod(root + "/codex.jsonl", 0o600); }
  expect(m.params).toHaveLength(4);
}));

for (const state of ["unknown", "spooled", "blocked", "quarantined"] as const) test(`agentlog ${state} recovery does not retransmit or advance`, () => fixture(async (root, cp) => {
  const m = mock(cp, true);
  await expect(ingestAgentLog(root, cp, m.client)).rejects.toThrow("lost reply");
  const before = await readFile(cp), pending = await readFile(cp + ".pending.json");
  const client = Object.assign(Object.create(RpcClient.prototype) as RpcClient, { request: async (method: string, input: object) => {
    if (method === "status") return { data_incarnation: incarnation };
    expect(method).toBe("ingest.status"); return { ...input, state };
  }});
  await expect(ingestAgentLog(root, cp, client)).rejects.toHaveProperty("code", `source_pending_${state}`);
  expect(await readFile(cp)).toEqual(before);
  expect(await readFile(cp + ".pending.json")).toEqual(pending);
}));
