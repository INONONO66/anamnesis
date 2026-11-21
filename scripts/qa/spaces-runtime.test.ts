import { expect, test } from "bun:test";
import { randomUUID } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { startProcess } from "./runtime-scenarios.ts";

test("built operator imports JSONL, rebuilds historical Episodes and retains assignments after restart", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("ownership-safe runner must inject Neo4j credentials");
  const root = await mkdtemp("/tmp/ana-spaces-"), token = randomUUID();
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token,
    ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password };
  // This scenario exercises operator and admission paths, with no external providers.
  for (const key of Object.keys(env)) if (/^ANAMNESIS_(LLM_|EXTRACTION_|EMBEDDING_|RPC_TCP)/.test(key)) delete env[key];
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const launch = () => startProcess("node", ["dist/anamnesis-daemon.mjs"], { env, deadlineMs: 60000, readyLine: /"event":"listening"/ });
  let daemon = launch(), client: RpcClient | undefined;
  const cli = async (...args: string[]) => {
    const result = await startProcess("node", ["dist/anamnesis-ops.mjs", ...args], { env, deadlineMs: 60000 }).done;
    expect(result.code, result.output).toBe(0);
    return result.output.trim().split("\n").filter(Boolean).map(line => JSON.parse(line));
  };
  const admit = async (record: string, reply?: string) => {
    if (!client) throw new Error("client not connected");
    const result = await client.request("remember", {
      episode: { schema: "anamnesis.original-message/1", content: record,
        time: { value: "2026-09-01T00:00:00Z", precision: "second" },
        origin: { source: "kakao", session: "room", actor: "user", record },
        properties: { room: "room", ...(reply ? { reply } : {}) } },
      source_revision: "v1", expected_previous_revision_key: null, origin_role: "user", lineage_mode: "direct", parent_recall_ids: [],
    });
    if (result.state !== "committed") throw new Error("fixture admission was not committed");
    return result.id;
  };
  try {
    expect(await daemon.ready).toBe(true);
    client = await RpcClient.connect(join(root, "anamnesis.sock"), token);
    const parent = await admit("parent"), reply = await admit("reply", "kakao:room#parent");
    const original = (await driver.executeQuery("MATCH (e:Episode {id:$id}) RETURN e.digest AS digest,e.properties AS properties,e.lineage_digest AS lineage", { id: reply })).records[0]?.toObject();
    await driver.executeQuery("MATCH (e:Episode) REMOVE e.room,e.space");
    await driver.executeQuery("MATCH ()-[r:REPLIES_TO]->() DELETE r");
    const file = join(root, "rooms.jsonl");
    // The client's rooms file: a name may hold U+2028, agent sessions carry a null name, extra keys ride along.
    await writeFile(file, [
      { source: "kakao", room: "room", space: "project:bori", name: "Chat\u2028room", ignored: true },
      { source: "omo", room: "session", space: "scratch:omo", name: null, records: 3 },
    ].map(row => JSON.stringify(row)).join("\n") + "\n");

    expect(await cli("spaces", "import", file)).toEqual([{ imported: 2, changed: 2 }]);
    expect((await cli("spaces", "rebuild", "--limit", "1")).reduce((sum, page) => sum + page.updated, 0)).toBe(2);
    expect((await cli("replies", "rebuild")).at(-1)).toMatchObject({ created: 1, missing: 0, done: true });
    expect(await cli("spaces", "list")).toEqual([{ space: "project:bori", episodes: 2, rooms: 1,
      first: "2026-09-01T00:00:00.000Z", last: "2026-09-01T00:00:00.000Z" }]);
    const stored = await driver.executeQuery("MATCH (e:Episode {id:$id})-[r:REPLIES_TO]->(p) RETURN e.digest AS digest,e.properties AS properties,e.lineage_digest AS lineage,p.id AS parent", { id: reply });
    expect(stored.records.map(record => record.toObject())).toEqual([{ ...original, parent }]);
    const invalid = await startProcess("node", ["dist/anamnesis-ops.mjs", "spaces", "rebuild", "--source", "kakao"], { env, deadlineMs: 60000 }).done;
    expect(invalid.code).toBe(1);
    expect(invalid.output).toContain("--source and --room must be supplied together");

    await client.request("shutdown", {}); await client.close(); client = undefined;
    expect((await daemon.done).code).toBe(0);
    daemon = launch(); expect(await daemon.ready).toBe(true);
    client = await RpcClient.connect(join(root, "anamnesis.sock"), token);
    const fresh = await admit("after-restart");
    const labels = await driver.executeQuery("MATCH (e:Episode {id:$id}) RETURN e.room AS room,e.space AS space", { id: fresh });
    expect(labels.records.map(record => record.toObject())).toEqual([{ room: "room", space: "project:bori" }]);
    expect(await cli("spaces", "import", file)).toEqual([{ imported: 2, changed: 0 }]);
  } finally {
    if (client) { await client.request("shutdown", {}); await client.close(); }
    daemon.stop(); await daemon.done;
    await driver.close(); await rm(root, { recursive: true, force: true });
  }
}, 120000);
