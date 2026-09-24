import { describe, expect, test } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { mkdtemp, mkdir, readFile, rm, symlink, writeFile } from "node:fs/promises";
import { join, relative } from "node:path";
import { ingestSlack } from "./slack.ts";
import { sourceRevisionKey } from "./source.ts";
import type { RpcClient } from "./client.ts";
import { RPC_LIMITS, type RpcRememberParams } from "../../packages/protocol/src/rpc.ts";

const hash = (v: string | Uint8Array) => createHash("sha256").update(v).digest("hex");
const canonical = (v: unknown): string => v === null || typeof v !== "object" ? JSON.stringify(v) : Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : `{${Object.entries(v).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([k, x]) => `${JSON.stringify(k)}:${canonical(x)}`).join(",")}}`;
const message = (text = "A", edited?: string) => ({ ts: "21.0", user: "U", text, ...(edited ? { edited: { ts: edited } } : {}) });

// Transport double only: the real adapter, pending files, replay and checkpoint
// seam run unchanged. Enforce body binding and CAS rather than modeled counts.
function transport() {
  const incarnation = randomUUID();
  const deliveries: RpcRememberParams[] = [];
  const bindings = new Map<string, { body_digest: string; revision_key: string; data_incarnation: string; state: "committed" }>();
  const heads = new Map<string, string>();
  const objects = new Map<string, Buffer>();
  let uploading: { sha256: string; size: number; media_type: string; chunks: Buffer[] };
  const methods: string[] = [];
  const client = { async request(method: string, p: any) {
    methods.push(method);
    if (method === "status") return { data_incarnation: incarnation };
    if (method === "ingest.status") return bindings.get(p.revision_key) ?? { ...p, state: "unknown", storage: "available" };
    if (method === "object.begin") { uploading = { ...p, chunks: [] }; return { state: "uploading", upload_id: randomUUID() }; }
    if (method === "object.chunk") { expect(p.seq).toBe(uploading.chunks.length); uploading.chunks.push(Buffer.from(p.bytes_b64, "base64")); return { next_seq: p.seq + 1 }; }
    if (method === "object.commit") { const body = Buffer.concat(uploading.chunks); expect(body.length).toBe(uploading.size); expect(hash(body)).toBe(uploading.sha256); objects.set(uploading.sha256, body); return { hash: uploading.sha256, size: body.length, media_type: uploading.media_type }; }
    if (method !== "remember") throw new Error(`unexpected method ${method}`);
    const params = p as RpcRememberParams;
    deliveries.push(params);
    const key = sourceRevisionKey(params), digest = hash(canonical({ digest_version: 1, params }));
    const prior = bindings.get(key);
    if (prior && prior.body_digest !== digest) throw new Error("revision_conflict");
    if (prior) return prior;
    const origin = canonical(params.episode.origin);
    if ((heads.get(origin) ?? null) !== params.expected_previous_revision_key) throw new Error("stale_revision");
    if (params.payload_hash) expect(objects.has(params.payload_hash)).toBe(true);
    const result = { revision_key: key, body_digest: digest, data_incarnation: incarnation, state: "committed" as const };
    bindings.set(key, result); heads.set(origin, key); return result;
  } } as unknown as RpcClient;
  return { client, deliveries, bindings, objects, methods };
}
async function fixture(lines: unknown[]) {
  const root = await mkdtemp("/tmp/slack-adapter-");
  await mkdir(join(root, "channels")); await mkdir(join(root, "threads"));
  await writeFile(join(root, "index.jsonl"), JSON.stringify({ id: "C1", name: "general" }) + "\n");
  await writeFile(join(root, "channels/C1.jsonl"), lines.map(x => JSON.stringify(x)).join("\n") + "\n");
  return { root, checkpoint: join(root, "checkpoint.json"), cleanup: () => rm(root, { recursive: true, force: true }) };
}
const saved = async (path: string) => JSON.parse(await readFile(path, "utf8"));

describe("ingest-slack immutable snapshot", () => {
  test("channel/thread native duplicates replay identical full params and predecessor", async () => {
    const f = await fixture([message()]); const t = transport();
    try {
      await writeFile(join(f.root, "threads/C1-21.0.jsonl"), JSON.stringify(message()) + "\n");
      await ingestSlack(f.root, f.checkpoint, t.client);
      expect(t.deliveries[1]).toEqual(t.deliveries[0]); expect(t.bindings.size).toBe(1);
      expect((await saved(f.checkpoint)).next).toBe(2);
      await ingestSlack(f.root, f.checkpoint, t.client); expect(t.deliveries.length).toBe(2);
    } finally { await f.cleanup(); }
  });
  test("first edited.ts and each unseen native revision stay native; A-B-A alone gets occurrence", async () => {
    const f = await fixture([message("A", "22.0"), message("B", "23.0"), message("B", "23.0"), message("A", "22.0"), message("A", "22.0")]); const t = transport();
    try {
      await ingestSlack(f.root, f.checkpoint, t.client);
      expect(t.deliveries.map(p => p.source_revision)).toEqual(["22.0", "23.0", "23.0", "22.0:occurrence:4", "22.0:occurrence:4"]);
      expect(t.deliveries.map(p => p.expected_previous_revision_key)).toEqual([null, sourceRevisionKey(t.deliveries[0]!), sourceRevisionKey(t.deliveries[0]!), sourceRevisionKey(t.deliveries[1]!), sourceRevisionKey(t.deliveries[1]!)]);
      expect(t.deliveries[1]).toEqual(t.deliveries[2]); expect(t.deliveries[3]).toEqual(t.deliveries[4]);
      expect(t.bindings.size).toBe(3); expect((await saved(f.checkpoint)).next).toBe(5);
    } finally { await f.cleanup(); }
  });
  test("conflicting same-native body is rejected before any delivery", async () => {
    const f = await fixture([message(), message("changed")]); const t = transport();
    try { await expect(ingestSlack(f.root, f.checkpoint, t.client)).rejects.toThrow("source_revision_conflict"); expect(t.methods).toEqual([]); }
    finally { await f.cleanup(); }
  });
  test("housekeeping, localized joins, blank lines, redaction and thread context", async () => {
    const types = ["channel_join", "channel_leave", "group_join", "group_leave", "channel_topic", "channel_purpose", "channel_name", "mpdm_move", "huddle_thread", "bot_message"];
    const f = await fixture([...types.map(subtype => ({ ...message(), subtype })), ...["<@U> has joined the channel", "<@U>さんがチャンネルに参加しました", "<@U>님이 채널에 참여했습니다"].map(text => message(text))]); const t = transport();
    try {
      await writeFile(join(f.root, "threads/C1-20.0.jsonl"), '\n  \r\n' + JSON.stringify({ ...message("token sk-abcdefghijklmnopqrstuvwxyz123456"), thread_ts: "20.0" }) + "\r\n");
      const losing = { request: async (method: string, params: any) => { const result = await t.client.request(method as "status", params); if (method === "remember") throw new Error("lost"); return result; } } as RpcClient;
      await expect(ingestSlack(relative(process.cwd(), f.root) + "/", f.checkpoint, losing)).rejects.toThrow("lost");
      expect(t.deliveries.length).toBe(1); expect(t.deliveries[0]!.episode.content).not.toContain("sk-abcdefghijklmnopqrstuvwxyz123456");
      expect(t.deliveries[0]!.episode.properties).toEqual({ channel_name: "general", slack_ts: "21.0", thread_parent_ts: "20.0" });
      expect((await saved(f.checkpoint + ".pending.json")).context).toEqual({ file: "threads/C1-20.0.jsonl", line: 3, native_source_revision: "21.0" });
    } finally { await f.cleanup(); }
  });
  test("long Unicode content uses redacted bounded preview and shared chunked payload upload", async () => {
    const text = "猫".repeat(200_000) + " sk-abcdefghijklmnopqrstuvwxyz123456";
    const f = await fixture([message(text)]); const t = transport();
    try {
      await ingestSlack(f.root, f.checkpoint, t.client);
      const p = t.deliveries[0]!; expect(Buffer.byteLength(p.episode.content)).toBeLessThanOrEqual(RPC_LIMITS.content_bytes);
      expect(p.episode.content).not.toContain("\uFFFD"); expect(p.payload_hash).toBeDefined();
      const body = t.objects.get(p.payload_hash!)!.toString(); expect(body.startsWith("猫".repeat(200_000))).toBe(true); expect(body).not.toContain("sk-abcdefghijklmnopqrstuvwxyz123456");
      expect(t.methods.filter(m => m === "object.chunk").length).toBe(2);
    } finally { await f.cleanup(); }
  });
  for (const [name, bytes, error] of [
    ["nonempty unterminated JSON", Buffer.from(JSON.stringify(message())), "source_partial_final_line"],
    ["malformed UTF8", Buffer.concat([Buffer.from('{"ts":"22.0","text":"'), Buffer.from([0xc3, 0x28]), Buffer.from('"}\n')]), "source_invalid_utf8"],
    ["malformed JSON", Buffer.from('{\n'), "source_invalid_record"],
    ["missing ts", Buffer.from('{"text":"bad"}\n'), "source_invalid_record"],
    ["oversized physical line", Buffer.alloc(8 * 1024 * 1024 + 1, 32), "source_record_too_large"],
  ] as const) test(`preflight all files: ${name} retains physical context and delivers nothing`, async () => {
    const f = await fixture([message()]); const t = transport();
    try {
      await writeFile(join(f.root, "threads/C1-22.0.jsonl"), Buffer.concat([Buffer.from("\n"), bytes]));
      await expect(ingestSlack(f.root, f.checkpoint, t.client)).rejects.toThrow(`${error}: threads/C1-22.0.jsonl:2`);
      expect(t.methods).toEqual([]);
    } finally { await f.cleanup(); }
  });
  for (const [name, bytes, error] of [
    ["partial", Buffer.from('{"id":"C1","name":"general"}'), "source_partial_final_line"],
    ["invalid UTF8", Buffer.from([0xff, 10]), "source_invalid_utf8"],
    ["metadata cap", Buffer.from(JSON.stringify({ id: "C1", name: "x".repeat(1025) }) + "\n"), "source_invalid_index"],
    ["total cap", Buffer.from("\n".repeat(4 * 1024 * 1024 + 1)), "source_index_too_large"],
  ] as const) test(`bounded index: ${name}`, async () => {
    const f = await fixture([message()]); const t = transport();
    try { await writeFile(join(f.root, "index.jsonl"), bytes); await expect(ingestSlack(f.root, f.checkpoint, t.client)).rejects.toThrow(error); expect(t.methods).toEqual([]); }
    finally { await f.cleanup(); }
  });
  test("symlinked export directory is not followed", async () => {
    const f = await fixture([message()]); const t = transport();
    try { await rm(join(f.root, "threads"), { recursive: true }); await symlink(join(f.root, "channels"), join(f.root, "threads")); await expect(ingestSlack(f.root, f.checkpoint, t.client)).rejects.toThrow("source_symlink"); expect(t.methods).toEqual([]); }
    finally { await f.cleanup(); }
  });
});
