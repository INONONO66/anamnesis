import { expect, setDefaultTimeout, test } from "bun:test";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";

setDefaultTimeout(60_000);
const ROOT = fileURLToPath(new URL("../../", import.meta.url));

test("receipt-mode commit is served over the daemon socket with replay, idempotency and mode refusals", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const { RpcClient } = await import(`${ROOT}dist/anamnesis-client.mjs`);
  const root = await mkdtemp("/tmp/anamnesis-commit-"), token = randomUUID();
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  await query("MATCH (n) DETACH DELETE n");
  const child = spawn("node", ["dist/anamnesis-daemon.mjs"], { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env,
    ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password } });
  const exited = once(child, "exit");
  child.stderr.on("data", chunk => process.stderr.write(chunk));
  const lines = createInterface({ input: child.stdout });
  const listening = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("daemon readiness deadline")), 20_000);
    lines.on("line", line => { if (JSON.parse(line).event === "listening") { clearTimeout(timer); resolve(); } });
    child.once("exit", code => { clearTimeout(timer); reject(new Error(`daemon exited ${code}`)); });
  });
  type Client = { request(method: string, params: unknown): Promise<any>; close(): Promise<void> };
  const clients: Client[] = [];
  const failure = (pending: Promise<unknown>) => pending.then(() => null, (error: { code: string }) => error.code);
  try {
    await listening;
    const receipt: Client = await RpcClient.connect(`${root}/anamnesis.sock`, token, "receipt"); clients.push(receipt);
    const auto: Client = await RpcClient.connect(`${root}/anamnesis.sock`, token, "auto"); clients.push(auto);
    const remembered = await receipt.request("remember", { episode: { schema: "anamnesis.original-message/1", content: "the commit wire marker",
      time: { value: "2026-09-01T00:00:00Z", precision: "second" }, mass: 0.5, properties: {}, origin: { source: root, session: "commit", actor: "fixture", record: "one" } },
      source_revision: "v1", expected_previous_revision_key: null });
    expect(remembered.state).toBe("committed");
    const recall = await receipt.request("recall", { query: "commit wire marker", limit: 5 });
    expect(recall.results.map((item: { id: string }) => item.id)).toEqual([remembered.id]);
    const cache = async () => Number((await query("MATCH (c:HitCache {episode_id:$id}) RETURN c.s AS s", { id: remembered.id }))[0]?.["s"] ?? 0);
    const before = await cache();

    const request = { operation_id: uuidv7(), recall_id: recall.recall_id, adopted: [remembered.id] };
    const applied = { operation_id: request.operation_id, recall_id: recall.recall_id, adopted: [remembered.id], reward: null };
    expect(await receipt.request("commit", request)).toEqual({ ...applied, applied: true });
    expect(await cache()).toBeGreaterThan(before);
    expect(await query("MATCH (h:Hit {kind:'recall_hit', namespace:$recall})-[:HIT_OF]->(:Episode {id:$id}) RETURN h.operation_id AS operation",
      { recall: recall.recall_id, id: remembered.id })).toEqual([{ operation: request.operation_id }]);
    const after = await cache();
    expect(await receipt.request("commit", request)).toEqual({ ...applied, applied: false });
    expect(await cache()).toBe(after);

    expect(await failure(receipt.request("commit", { ...request, operation_id: uuidv7(), recall_id: uuidv7() }))).toBe("unknown_recall");
    expect(await failure(receipt.request("commit", { ...request, operation_id: uuidv7(), adopted: [uuidv7()] }))).toBe("invalid_selection");
    expect(await failure(receipt.request("commit", { ...request, reward: 0 }))).toBe("idempotency_conflict");
    expect(await failure(auto.request("commit", { ...request, operation_id: uuidv7() }))).toBe("commit_mode_mismatch");
    expect(await query("MATCH (h:Hit {kind:'recall_hit'}) RETURN count(h) AS hits")).toEqual([{ hits: 1 }]);

    await receipt.request("shutdown", {});
    expect((await exited)[0]).toBe(0);
  } finally {
    await Promise.allSettled(clients.map(client => client.close()));
    if (child.exitCode === null && child.signalCode === null) { child.kill("SIGKILL"); await exited; }
    await driver.close();
    await rm(root, { recursive: true, force: true });
  }
});
