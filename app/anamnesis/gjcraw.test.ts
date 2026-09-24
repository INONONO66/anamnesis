import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, rm, stat, symlink, writeFile } from "node:fs/promises";
import { RpcClient } from "./client.ts";
import { ingestGjcRaw } from "./gjcraw.ts";

const incarnation = "11111111-1111-4111-8111-111111111111";
const line = (v: object) => JSON.stringify(v) + "\n";
const session = line({ type: "session", id: "sess-native" });
const message = (id: string, text: string, timestamp = "2026-01-01T00:00:00Z") => line({ type: "message", id, timestamp, message: { role: "user", content: [{ type: "text", text }, { type: "thinking", text: "scratch" }, { type: "toolCall", text: "tool" }] } });
async function fixture(body: string, run: (source: string, cp: string, file: string) => Promise<void>) {
  const root = await mkdtemp("/tmp/ana-gjc-");
  const file = root + "/home/.gjc/agent/sessions/session.jsonl", cp = root + "-checkpoint.json";
  const owned = [root, cp, cp + ".pending.json", cp + ".lease"];
  try {
    await mkdir(file.slice(0, file.lastIndexOf("/")), { recursive: true });
    await writeFile(file, body);
    await run(root, cp, file);
  } finally {
    await Promise.all(owned.map(path => rm(path, { recursive: true, force: true })));
    for (const path of owned) await expect(stat(path)).rejects.toMatchObject({ code: "ENOENT" });
    console.log(JSON.stringify({ event: "unit_fixture_cleanup", family: "gjc", owned, remaining: [] }));
  }
}
function client(cp: string, state = "committed") { const methods: string[] = []; const seen = new Map<string, any>(); const c = Object.assign(Object.create(RpcClient.prototype), { request: async (method: string, input: any) => { methods.push(method); if (method === "status") return { data_incarnation: incarnation, storage: "available" }; if (method === "ingest.status") { const s = seen.get(JSON.stringify(input))?.state ?? state; return { ...input, state: s, ...(s === "unknown" ? { storage: "available" } : {}) }; } if (method === "remember") { const p = JSON.parse(await readFile(cp + ".pending.json", "utf8")); const result = { ...p.identity, state: "committed" }; seen.set(JSON.stringify(p.identity), result); return result; } throw new Error(`unexpected ${method}`); } }); return { client: c, methods }; }

test("session inheritance, joined text, compaction, exclusions, masking, payload, IDs and revisions", async () => fixture(session + message("A", "hello\npassword=supersecret", "2026-01-01T00:00:00Z") + line({ type: "compaction", id: "C", timestamp: "2026-01-01T00:00:01Z", summary: "old context" }) + line({ type: "message", id: "T", timestamp: "2026-01-01T00:00:02Z", message: { role: "toolResult", content: [{ type: "text", text: "ignored" }] } }), async (source, cp) => { const m = client(cp); await ingestGjcRaw(source, cp, m.client as any); expect((await JSON.parse(await readFile(cp, "utf8"))).next).toBe(2); expect(m.methods.filter(x => x === "remember")).toHaveLength(2); const pending = await stat(cp).then(() => true); expect(pending).toBe(true); }));

test("A-B-A preserves observed occurrence and immutable boundaries", async () => fixture(session + message("A", "A") + message("B", "B", "2026-01-01T00:00:01Z") + message("A", "A", "2026-01-01T00:00:02Z"), async (source, cp, file) => { const m = client(cp); await ingestGjcRaw(source, cp, m.client as any); const before = await readFile(cp); await ingestGjcRaw(source, cp, m.client as any); expect(await readFile(cp)).toEqual(before); await writeFile(file, session + message("A", "changed")); await expect(ingestGjcRaw(source, cp, m.client as any)).rejects.toThrow("source_changed"); }));

for (const [name, body, error] of [["partial", session + JSON.stringify({ type: "message" }), "source_partial_final_line"], ["malformed", session + "{bad}\n", "source_invalid_record"], ["utf8", Buffer.concat([Buffer.from(session), Buffer.from([0xc3, 0x28, 10])]), "source_invalid_utf8"]] as const) test(`${name} is rejected before RPC`, async () => fixture(body as any, async (source, cp) => { const m = client(cp); await expect(ingestGjcRaw(source, cp, m.client as any)).rejects.toThrow(error); expect(m.methods).toEqual([]); }));

test("symlink, permission and checkpoint boundary are explicit", async () => fixture(session + message("A", "x"), async (source, cp) => { await symlink(source + "/home", source + "/link"); const m = client(cp); await expect(ingestGjcRaw(source, cp, m.client as any)).rejects.toThrow("source_symlink"); await rm(source + "/link"); await expect(ingestGjcRaw(source, source + "/checkpoint.json", m.client as any)).rejects.toThrow("source_checkpoint_path_conflict"); }));

test("spooled and foreign-incarnation outcomes never retransmit or advance; unknown from the same ready incarnation is retired and resent", async () => fixture(session + message("A", "x"), async (source, cp) => {
  const first = client(cp); first.client.request = async (method: string) => { if (method === "status") return { data_incarnation: incarnation, storage: "available" }; throw new Error("lost reply"); };
  await expect(ingestGjcRaw(source, cp, first.client as any)).rejects.toThrow("lost reply");
  const before = await readFile(cp), pending = await readFile(cp + ".pending.json");
  const spooled = client(cp, "spooled"); await expect(ingestGjcRaw(source, cp, spooled.client as any)).rejects.toThrow("source_pending_spooled"); expect(spooled.methods).toEqual(["status", "ingest.status"]);
  const foreign = client(cp, "unknown"); foreign.client.request = async (method: string) => { expect(method).toBe("status"); return { data_incarnation: "33333333-3333-4333-8333-333333333333", storage: "available" }; };
  await expect(ingestGjcRaw(source, cp, foreign.client as any)).rejects.toThrow("incarnation_mismatch");
  expect(await readFile(cp)).toEqual(before); expect(await readFile(cp + ".pending.json")).toEqual(pending);
  // The daemon holds nothing for the lost delivery: UNKNOWN from the same ready
  // incarnation retires the pending record and resends it from the checkpoint.
  const m = client(cp, "unknown"); await ingestGjcRaw(source, cp, m.client as any);
  expect(m.methods).toEqual(["status", "ingest.status", "remember"]);
  await expect(stat(cp + ".pending.json")).rejects.toMatchObject({ code: "ENOENT" }); expect((JSON.parse(await readFile(cp, "utf8"))).next).toBe(1);
}));
