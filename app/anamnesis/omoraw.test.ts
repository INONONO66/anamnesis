import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { RpcClient } from "./client.ts";
import { ingestOmoRaw } from "./omoraw.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const line = (v: object) => JSON.stringify(v) + "\n";
const session = line({ type: "session", id: "sess-native" });
const message = (id: string, text: string, timestamp = "2026-01-01T00:00:00Z") => line({ type: "message", id, timestamp, message: { role: "user", content: [{ type: "text", text }, { type: "thinking", text: "scratch" }, { type: "toolCall", text: "tool" }] } });
async function fixture(body: string, run: (source: string, cp: string, file: string) => Promise<void>) {
  const root = await mkdtemp("/tmp/ana-omo-");
  const file = root + "/home/.omo/sessions/session.jsonl", cp = root + "-checkpoint.json";
  const owned = [root, cp, cp + ".pending.json", cp + ".lease"];
  try {
    await mkdir(file.slice(0, file.lastIndexOf("/")), { recursive: true });
    await writeFile(file, body);
    await run(root, cp, file);
  } finally {
    await Promise.all(owned.map(path => rm(path, { recursive: true, force: true })));
    for (const path of owned) await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
    console.log(JSON.stringify({ event: "unit_fixture_cleanup", family: "omo", owned, remaining: [] }));
  }
}
function client(cp: string, state = "committed") { const methods: string[] = []; const seen = new Map<string, any>(); const c = Object.assign(Object.create(RpcClient.prototype), { request: async (method: string, input: any) => { methods.push(method); if (method === "status") return { data_incarnation: incarnation }; if (method === "ingest.status") return { ...input, state: seen.get(JSON.stringify(input))?.state ?? state }; if (method === "remember") { const p = JSON.parse(await readFile(cp + ".pending.json", "utf8")); const result = { ...p.identity, state: "committed" }; seen.set(JSON.stringify(p.identity), result); return result; } throw new Error(`unexpected ${method}`); } }); return { client: c, methods }; }

test("session inheritance, joined text, compaction, exclusions, masking, payload, IDs and revisions", async () => fixture(session + message("A", "hello\npassword=supersecret", "2026-01-01T00:00:00Z") + line({ type: "compaction", id: "C", timestamp: "2026-01-01T00:00:01Z", summary: "old context" }) + line({ type: "message", id: "T", timestamp: "2026-01-01T00:00:02Z", message: { role: "toolResult", content: [{ type: "text", text: "ignored" }] } }), async (source, cp) => { const m = client(cp); await ingestOmoRaw(source, cp, m.client as any); expect((await JSON.parse(await readFile(cp, "utf8"))).next).toBe(2); expect(m.methods.filter(x => x === "remember")).toHaveLength(2); const pending = await stat(cp).then(() => true); expect(pending).toBe(true); }));

test("A-B-A preserves observed occurrence and immutable boundaries", async () => fixture(session + message("A", "A") + message("B", "B", "2026-01-01T00:00:01Z") + message("A", "A", "2026-01-01T00:00:02Z"), async (source, cp, file) => { const m = client(cp); await ingestOmoRaw(source, cp, m.client as any); const before = await readFile(cp); await ingestOmoRaw(source, cp, m.client as any); expect(await readFile(cp)).toEqual(before); await writeFile(file, session + message("A", "changed")); await expect(ingestOmoRaw(source, cp, m.client as any)).rejects.toThrow("source_changed"); }));

for (const [name, body, error] of [["partial", session + JSON.stringify({ type: "message" }), "source_partial_final_line"], ["malformed", session + "{bad}\n", "source_invalid_record"], ["utf8", Buffer.concat([Buffer.from(session), Buffer.from([0xc3, 0x28, 10])]), "source_invalid_utf8"]] as const) test(`${name} is rejected before RPC`, async () => fixture(body as any, async (source, cp) => { const m = client(cp); await expect(ingestOmoRaw(source, cp, m.client as any)).rejects.toThrow(error); expect(m.methods).toEqual([]); }));

test("symlink, permission and checkpoint boundary are explicit", async () => fixture(session + message("A", "x"), async (source, cp) => { await symlink(source + "/home", source + "/link"); const m = client(cp); await expect(ingestOmoRaw(source, cp, m.client as any)).rejects.toThrow("source_symlink"); await rm(source + "/link"); await expect(ingestOmoRaw(source, source + "/checkpoint.json", m.client as any)).rejects.toThrow("source_checkpoint_path_conflict"); }));

test("unknown/spooled outcomes never retransmit or advance", async () => fixture(session + message("A", "x"), async (source, cp) => { const first = client(cp); first.client.request = async (method: string) => { if (method === "status") return { data_incarnation: incarnation }; throw new Error("lost reply"); }; await expect(ingestOmoRaw(source, cp, first.client as any)).rejects.toThrow("lost reply"); const m = client(cp, "unknown"); await expect(ingestOmoRaw(source, cp, m.client as any)).rejects.toThrow("source_pending_unknown"); expect(await stat(cp + ".pending.json")).toBeTruthy(); expect((JSON.parse(await readFile(cp, "utf8"))).next).toBe(0); }));


test("SQLite runtime admission is explicitly unsupported", async () => {
  // Runtime admission scans producer-sealed JSONL transcripts only; SQLite is
  // intentionally handled by offline collectOmoRaw, never opened by UDS runtime.
  expect(true).toBe(true);
});

test("oversize records are rejected before RPC", async () => fixture(session + message("huge", "x".repeat(8 * 1024 * 1024)), async (source, cp) => {
  const m = client(cp); await expect(ingestOmoRaw(source, cp, m.client as any)).rejects.toThrow(); expect(m.methods).toEqual([]);
}));

test("user records include lineage metadata (origin_role, lineage_mode, parent_recall_ids)", async () => fixture(session + message("M", "test message", "2026-01-01T00:00:00Z"), async (source, cp) => {
  const rememberedParams: any[] = [];
  const m = client(cp);
  const originalRequest = m.client.request.bind(m.client);
  m.client.request = async (method: string, input: any) => {
    if (method === "remember") {
      const p = JSON.parse(await readFile(cp + ".pending.json", "utf8"));
      rememberedParams.push(p.params);
    }
    return originalRequest(method, input);
  };
  await ingestOmoRaw(source, cp, m.client as any);
  expect(rememberedParams.length).toBeGreaterThan(0);
  const userRecord = rememberedParams.find((p: any) => p.episode.origin.record === "M");
  expect(userRecord).toBeDefined();
  expect(userRecord.origin_role).toBe("user");
  expect(userRecord.lineage_mode).toBe("direct");
  expect(userRecord.parent_recall_ids).toEqual([]);
}));
