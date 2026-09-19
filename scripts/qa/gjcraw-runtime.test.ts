import { expect, test } from "bun:test";
import { execFileSync } from "node:child_process";
import { lstat, mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { randomUUID } from "node:crypto";
import neo4j from "neo4j-driver";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { startProcess } from "./runtime-scenarios.ts";

for (const failAfterIngest of [false, true]) test(`GJC raw private Node daemon/ops UDS runtime${failAfterIngest ? " fixture failure cleanup" : ""}`, async () => {
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("runner credentials missing");
  const evidence = resolve(".omo/evidence/gjcraw-runtime/real");
  await mkdir(evidence, { recursive: true });
  const build = join(evidence, `bundle-${randomUUID()}`);
  execFileSync(process.execPath, ["build", "app/anamnesis/main.ts", "--target=node", "--outfile", `${build}-main.mjs`]);
  execFileSync(process.execPath, ["build", "app/anamnesis/ops.ts", "--target=node", "--outfile", `${build}-ops.mjs`]);
  const root = await mkdtemp("/tmp/ana-gjc-runtime-"), source = join(root, "export"), cp = root + "-checkpoint.json";
  const owned = [root, cp, cp + ".pending.json", cp + ".lease"];
  let daemon: ReturnType<typeof startProcess> | undefined;
  let client: RpcClient | undefined;
  const injected = new Error("fixture_failure_after_ingest");
  const run = async () => {
    try {
      await mkdir(join(source, "home/.gjc/agent/sessions"), { recursive: true });
      const file = join(source, "home/.gjc/agent/sessions/session.jsonl");
      await writeFile(file, JSON.stringify({ type: "session", id: "runtime-session" }) + "\n" + JSON.stringify({ type: "message", id: "native-a", timestamp: "2026-01-01T00:00:00Z", message: { role: "user", content: [{ type: "text", text: "runtime A" }] } }) + "\n" + JSON.stringify({ type: "compaction", id: "native-c", timestamp: "2026-01-01T00:00:01Z", summary: "context" }) + "\n");
      const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: randomUUID(), ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_USER: process.env.ANAMNESIS_TEST_NEO4J_USER ?? "neo4j", ANAMNESIS_NEO4J_PASSWORD: password };
      daemon = startProcess("node", [`${build}-main.mjs`], { env, deadlineMs: 60000, readyLine: /event.*listening/ });
      expect(await daemon.ready).toBe(true);
      client = await RpcClient.connect(join(root, "anamnesis.sock"), env.ANAMNESIS_RUNTIME_TOKEN);
      const ops = () => startProcess("node", [`${build}-ops.mjs`, "ingest-gjc-raw", source, cp], { env, deadlineMs: 120000 });
      const first = await ops().done;
      expect(first.code).toBe(0);
      const checkpoint = JSON.parse(await readFile(cp, "utf8"));
      expect(checkpoint.next).toBe(2);
      if (failAfterIngest) {
        // Test-owned pending artifact; this injection tests teardown, not recovery.
        await writeFile(cp + ".pending.json", "fixture-owned pending\n");
        throw injected; // Before driver construction: the client must still close.
      }
      const duplicate = await ops().done;
      expect(duplicate.code).toBe(0);
      expect(JSON.parse(await readFile(cp, "utf8")).next).toBe(2);
      const driver = neo4j.driver(uri, neo4j.auth.basic(env.ANAMNESIS_NEO4J_USER, password), { disableLosslessIntegers: true });
      try {
        const rows = (await driver.executeQuery("MATCH (e:Episode {origin_source:'gjc'}) RETURN e.origin_record AS record,e.origin_session AS session,e.content AS content,e.source_revision AS revision,e.ingest_seq AS seq ORDER BY e.ingest_seq")).records.map(r => r.toObject());
        expect(rows).toHaveLength(2);
        expect(rows.map(r => r.record)).toEqual(["native-a", "native-c"]);
        const effects = (await driver.executeQuery("MATCH (e:Episode {origin_source:'gjc'}) OPTIONAL MATCH (o:Outbox {element_id:e.id}) RETURN count(o) AS effects")).records[0]!.get("effects");
        expect(Number(effects)).toBe(2);
        await writeFile(join(evidence, "result.json"), JSON.stringify({ ok: true, rows, checkpoint, duplicate_noop: true }, null, 2));
      } finally { await driver.close(); }
    } finally {
      try { if (client) await client.close(); }
      finally {
        try { if (daemon) { daemon.stop(); await daemon.done; } }
        finally {
          const before = await Promise.all(owned.map(async path => {
            try { await lstat(path); return { path, exists: true }; }
            catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; return { path, exists: false }; }
          }));
          await Promise.all(owned.map(path => rm(path, { recursive: true, force: true })));
          for (const path of owned) await expect(lstat(path)).rejects.toMatchObject({ code: "ENOENT" });
          const receipt = { event: "runtime_fixture_cleanup", family: "gjc", failAfterIngest, owned, before, remaining: [], client_closed: !!client, daemon_stopped: !!daemon };
          console.log(JSON.stringify(receipt));
          await writeFile(`${build}-cleanup.json`, JSON.stringify(receipt, null, 2));
        }
      }
    }
  };
  if (failAfterIngest) await expect(run()).rejects.toBe(injected);
  else await run();
}, 180000);
