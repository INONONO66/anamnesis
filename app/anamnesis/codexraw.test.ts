import { expect, test } from "bun:test";
import { mkdtemp, mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { createCodexRawParser } from "../../packages/backfill/src/codexraw.ts";
import { ingestCodexRaw } from "./codexraw.ts";
import { RpcClient } from "./client.ts";

const rec = (timestamp: string, type: string, payload: object) => JSON.stringify({ timestamp, type, payload });
const meta = rec("2026-01-01T00:00:00Z", "session_meta", { id: "s", cwd: "/work" });
const ctx = rec("2026-01-01T00:00:00Z", "turn_context", { model: "gpt-test" });
const msg = (text: string, id?: string, role = "user") => rec("2026-01-01T00:00:01Z", "response_item", { type: "message", role, ...(id ? { id } : {}), content: [{ text }] });

test("Codex parser preserves metadata, messages, compaction, fallback IDs, and excludes plumbing", () => {
  const parse = createCodexRawParser("fallback", true);
  const out = [meta, ctx, msg("A", "native"), rec("2026-01-01T00:00:02Z", "event_msg", { type: "agent_message", message: "B", role: "assistant" }), rec("2026-01-01T00:00:03Z", "response_item", { type: "context_compacted" }), rec("2026-01-01T00:00:04Z", "response_item", { type: "function_call", name: "tool" }), msg("system", "sys", "system"), msg("developer", "dev", "developer"), msg("fallback")].flatMap(line => parse(line));
  expect(out.map(x => x.input.content)).toEqual(["A", "B", "context_compacted", "fallback"]);
  expect(out[0]!.input.origin.record).toBe("native:content:0");
  expect(out[1]!.input.origin.record).toBe("s:3");
  expect(out[0]!.input.properties).toMatchObject({ cwd: "/work", model: "gpt-test" });
});

test("strict parser rejects malformed and invalid records", () => {
  const parse = createCodexRawParser("s", true);
  expect(() => parse("{bad}")).toThrow();
  expect(() => parse(JSON.stringify({ timestamp: "x", type: "x" }))).toThrow();
  expect(() => parse(new TextDecoder().decode(Uint8Array.from([0xc3, 0x28])))).toThrow();
});

test("runtime adapter streams a sealed export and commit-gates checkpoint", async () => {
  const root = await mkdtemp("/tmp/ana-codex-test-"); const source = join(root, "export"); const cp = join(root, "checkpoint.json");
  await mkdir(source); await writeFile(join(source, "rollout-2026-01-01-s.jsonl"), meta + "\n" + ctx + "\n" + msg("x", "id") + "\n");
  const committed = new Map<string, any>(); const calls: string[] = [];
  const client = Object.assign(Object.create(RpcClient.prototype), { request: async (method: string, p: any) => { calls.push(method); if (method === "status") return { data_incarnation: "11111111-1111-4111-8111-111111111111" }; if (method === "ingest.status") return committed.get(p.revision_key) ?? { ...p, state: "unknown", storage: "available" }; if (method === "remember") { const pending = JSON.parse(await Bun.file(cp + ".pending.json").text()); const r = { ...pending.identity, state: "committed", created: true, id: "id", ingest_seq: 1 }; committed.set(p.revision_key, r); return r; } throw new Error(method); } });
  try { await ingestCodexRaw(source, cp, client as RpcClient); expect(calls).toContain("remember"); expect(JSON.parse(await Bun.file(cp).text()).next).toBe(1); } finally { await rm(root, { recursive: true, force: true }); }
});

test("user and assistant records include lineage metadata (origin_role, lineage_mode, parent_recall_ids)", async () => {
  const root = await mkdtemp("/tmp/ana-codex-test-"); const source = join(root, "export"); const cp = join(root, "checkpoint.json");
  await mkdir(source);
  await writeFile(join(source, "rollout-2026-01-01-s.jsonl"), [meta, ctx, msg("user message", "u1", "user"), msg("assistant message", "a1", "assistant")].join("\n") + "\n");
  const committed = new Map<string, any>(); const remembered: any[] = [];
  const client = Object.assign(Object.create(RpcClient.prototype), { request: async (method: string, p: any) => {
    if (method === "status") return { data_incarnation: "11111111-1111-4111-8111-111111111111" };
    if (method === "ingest.status") return committed.get(p.revision_key) ?? { ...p, state: "unknown", storage: "available" };
    if (method === "remember") {
      const pending = JSON.parse(await Bun.file(cp + ".pending.json").text());
      remembered.push(pending.params);
      const r = { ...pending.identity, state: "committed", created: true, id: "id", ingest_seq: 1 };
      committed.set(p.revision_key, r);
      return r;
    }
    throw new Error(method);
  } });
  try {
    await ingestCodexRaw(source, cp, client as RpcClient);
    expect(remembered.length).toBe(2);
    const userRecord = remembered.find((p: any) => p.episode.content === "user message");
    expect(userRecord).toBeDefined();
    expect(userRecord.origin_role).toBe("user");
    expect(userRecord.lineage_mode).toBe("direct");
    expect(userRecord.parent_recall_ids).toEqual([]);
    const assistantRecord = remembered.find((p: any) => p.episode.content === "assistant message");
    expect(assistantRecord).toBeDefined();
    expect(assistantRecord.origin_role).toBe("assistant");
    expect(assistantRecord.lineage_mode).toBe("direct");
    expect(assistantRecord.parent_recall_ids).toEqual([]);
  } finally { await rm(root, { recursive: true, force: true }); }
});
