// Real owned runtime acceptance. Run only via runtime-scenarios.ts; that runner
// provisions Neo4j and injects credentials, then clears/removes its container.
import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { randomUUID, createHash } from "node:crypto";
import neo4j from "neo4j-driver";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { startProcess } from "./runtime-scenarios.ts";

const line = (timestamp: string, type: string, payload: object) => JSON.stringify({ timestamp, type, payload }) + "\n";
const sha = (v: string | Uint8Array) => createHash("sha256").update(v).digest("hex");
test("Codex raw immutable snapshot through private Node daemon/ops and UDS", async () => {
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("runner credentials missing");
  const evidence = resolve(".omo/evidence/codexraw-runtime"); await mkdir(evidence, { recursive: true });
  const build = join(evidence, `bundles-${randomUUID()}`); execFileSync("bun", ["build", "app/anamnesis/main.ts", "--target=node", "--outfile", `${build}-main.mjs`]); execFileSync("bun", ["build", "app/anamnesis/ops.ts", "--target=node", "--outfile", `${build}-ops.mjs`]);
  const root = await mkdtemp("/tmp/ana-codex-runtime-"); const source = join(root, "export"), cp = join(root, "checkpoint.json"); await mkdir(source);
  const rollout = line("2026-01-01T00:00:00Z", "session_meta", { id: "runtime-session", cwd: "/owned" }) + line("2026-01-01T00:00:00Z", "turn_context", { model: "gpt-test" }) + line("2026-01-01T00:00:01Z", "response_item", { type: "message", role: "user", id: "native-a", content: [{ text: "runtime A" }] }) + line("2026-01-01T00:00:02Z", "response_item", { type: "context_compacted" }) + line("2026-01-01T00:00:03Z", "response_item", { type: "function_call", name: "excluded" });
  await writeFile(join(source, "rollout-2026-01-01-runtime.jsonl"), rollout);
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: randomUUID(), ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_USER: process.env.ANAMNESIS_TEST_NEO4J_USER ?? "neo4j", ANAMNESIS_NEO4J_PASSWORD: password };
  const daemon = startProcess("node", [`${build}-main.mjs`], { env, deadlineMs: 60000, readyLine: /event.*listening/ });
  try {
    expect(await daemon.ready).toBe(true); const client = await RpcClient.connect(join(root, "anamnesis.sock"), env.ANAMNESIS_RUNTIME_TOKEN!);
    const status = await client.request("status", {}); expect(status.storage).toBe("available");
    const ops = async () => startProcess("node", [`${build}-ops.mjs`, "ingest-codex-raw", source, cp], { env, deadlineMs: 120000 });
    const first = await (await ops()).done; expect(first.code).toBe(0); expect((JSON.parse(await readFile(cp, "utf8"))).next).toBe(2);
    const duplicate = await (await ops()).done; expect(duplicate.code).toBe(0); expect((JSON.parse(await readFile(cp, "utf8"))).next).toBe(2);
    const driver = neo4j.driver(uri, neo4j.auth.basic(env.ANAMNESIS_NEO4J_USER!, password), { disableLosslessIntegers: true });
    try { const rows = (await driver.executeQuery("MATCH (e:Episode {origin_source:'codex'}) RETURN e.origin_record AS record,e.source_revision AS revision,e.origin_session AS session,e.content AS content,e.properties AS properties,e.ingest_seq AS sequence ORDER BY e.ingest_seq")).records.map(r => r.toObject()); expect(rows).toHaveLength(2); expect(rows).toMatchObject([{ record: "native-a:content:0", revision: "5e7de20e88f63036708a85983d7179d4dad85d6ebcdc5fae6f93536c83e16a23", session: "runtime-session", content: "runtime A" }, { record: "runtime-session:3", revision: "40d05ed1976ff39852c84fe9b8391d668cf93c3f45d98be85ab54e599034fbb8", session: "runtime-session", content: "context_compacted" }]); const effects = (await driver.executeQuery("MATCH (e:Episode {origin_source:'codex'}) OPTIONAL MATCH (o:Outbox {element_id:e.id}) WITH e, count(o) AS effects RETURN e.id AS id,e.ingest_seq AS sequence,effects ORDER BY sequence")).records.map(r => r.toObject()); expect(effects).toHaveLength(2); expect(effects.every(r => Number(r.effects) === 1)).toBe(true); await writeFile(join(evidence, "result.json"), JSON.stringify({ ok: true, rows, effects, duplicate_count_unchanged: true, checkpoint: JSON.parse(await readFile(cp, "utf8")), outbox: (await client.request("status", {})).outbox_pending }, null, 2)); }
    finally { await driver.close(); await client.close(); }
  } finally { daemon.stop(); await daemon.done; await rm(root, { recursive: true, force: true }); await writeFile(join(evidence, "cleanup.json"), JSON.stringify({ root_removed: true, daemon_stopped: true })); }
}, 180000);
