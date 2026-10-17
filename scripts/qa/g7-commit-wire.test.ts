import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { once } from "node:events";
import { createServer, type Server } from "node:http";
import type { Socket } from "node:net";
import { createInterface, type Interface } from "node:readline";
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OUT = process.env["ANAMNESIS_G7_SURFACE_OUT"]
  ?? "/Users/ino/Develop/anamnesis/.omo/evidence/ulw/01a11b8c-9f0b-71f3-ac0c-cb1048614a4e/G002-g7-reinforcement-commit-commit-moves/surface";
const profile = { model: "g7-wiring-fixture-v1", model_incarnation: "a".repeat(64), dimensions: 3,
  document_prefix: "document: ", query_prefix: "query: ", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 };
const extraction = { model: "g7-extraction-fixture", model_incarnation: "b".repeat(64) };
type Item = { id: string; kind: string; mass: number };
type Recall = { recall_id: string | null; results: Item[]; companions: Item[] };
type Client = { request(method: string, params: unknown): Promise<any>; close(): Promise<void> };

/** Deterministic local provider: one whole-text claim per Episode, every claim retained, a fixed vector per input. */
function providerServer(): { server: Server; sockets: Set<Socket> } {
  const sockets = new Set<Socket>();
  const server = createServer(async (req, res) => {
    const chunks: Buffer[] = []; for await (const chunk of req) chunks.push(chunk as Buffer);
    const request = JSON.parse(Buffer.concat(chunks).toString());
    res.setHeader("content-type", "application/json");
    if (!request.task) {
      res.end(JSON.stringify({ ...profile, data: [{ index: 0, embedding: String(request.input).startsWith("query: ") ? [1, 0, 0] : [0, 1, 0] }] }));
      return;
    }
    const claims = (text: string) => [{ text, evidence: { start: 0, end: Buffer.byteLength(text), text }, confidence: 0.9,
      entities: [{ mention: text.split(/\s/)[0], normalized_name: text.split(/\s/)[0], entity_kind: "person" }] }];
    const output = request.task === "claim_window"
      ? { task: "claim_window", episodes: request.episodes.map(({ id, text }: { id: string; text: string }) => ({ id, language: "en", modality: "text", claims: claims(text) })) }
      : request.task === "judge_claims"
        ? { task: "judge_claims", language: "en", modality: "text", claim_body_digest: request.claim_context.body_digest,
          decisions: request.claim_context.claims.map((claim: { evidence: unknown }, claim_index: number) => ({ claim_index, disposition: "retain", evidence: claim.evidence, confidence: 0.8 })) }
        : { task: "judge_relations", language: "en", modality: "text", relation_context_digest: request.relation_context.body_digest,
          judgements: request.relation_context.candidates.map((candidate: { id: string }) => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.9, reason: "fixture" })) };
    res.end(JSON.stringify({ ...extraction, output }));
  });
  server.on("connection", socket => { sockets.add(socket); socket.once("close", () => sockets.delete(socket)); });
  return { server, sockets };
}

test("receipt-mode commit reinforces the adopted Episode on the real daemon over UDS", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const { RpcClient } = await import(`${ROOT}dist/anamnesis-client.mjs`);
  const root = await mkdtemp("/tmp/g7-commit-wire-"), token = randomUUID();
  await mkdir(OUT, { recursive: true });
  const { server, sockets } = providerServer();
  const listening = once(server, "listening", { signal: AbortSignal.timeout(5000) }); server.listen(0, "127.0.0.1"); await listening;
  const address = server.address();
  if (!address || typeof address === "string") throw new Error("provider address");
  const endpoint = `http://127.0.0.1:${address.port}/v1/embeddings`;
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string, params: Record<string, unknown> = {}) => (await driver.executeQuery(cypher, params)).records.map(row => row.toObject());
  const child = spawn("node", ["dist/anamnesis-daemon.mjs"], { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env,
    ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password,
    ANAMNESIS_EMBEDDING_CONFIG: JSON.stringify({ endpoint, profile, timeout_ms: 5000 }),
    ANAMNESIS_EXTRACTION_CONFIG: JSON.stringify({ endpoint, ...extraction, timeout_ms: 5000 }) } });
  const exited = once(child, "exit");
  child.stderr.on("data", chunk => process.stderr.write(chunk));
  const lines: Interface = createInterface({ input: child.stdout });
  const daemonEvent = (name: string) => new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => { lines.off("line", onLine); reject(Error(`${name} deadline`)); }, 20000);
    const onLine = (line: string) => { if (JSON.parse(line).event === name) { clearTimeout(timer); lines.off("line", onLine); resolve(); } };
    lines.on("line", onLine);
  });
  const clients: Client[] = [];
  try {
    await query("MATCH (n) DETACH DELETE n");
    const ready = daemonEvent("listening");
    await Promise.race([ready, exited.then(([code]) => { throw Error(`daemon exit ${code}`); })]);
    const rpc: Client = await RpcClient.connect(`${root}/anamnesis.sock`, token, "receipt"); clients.push(rpc);
    const record = async (name: string, body: unknown) => writeFile(`${OUT}/${name}.json`, JSON.stringify(body, null, 2) + "\n");
    const failure = (pending: Promise<unknown>) => pending.then(() => null,
      (error: { code: string; message: string; retryable: boolean }) => ({ data_code: error.code, message: error.message, retryable: error.retryable }));
    const cache = async () => Object.fromEntries((await query("MATCH (c:HitCache) RETURN c.episode_id AS id,c.s AS s ORDER BY id"))
      .map(row => [String(row["id"]), row["s"] as number | null]));

    const helloRequest = { token: "<redacted>", client: "node-client", commit_mode: "receipt", version: 1 };
    const status = await rpc.request("status", {});
    await record("00-capabilities", { hello_request: helloRequest, status_request: {}, status_response: status });
    expect(status.capabilities.commit).toBe(true);
    expect(status.capabilities.methods).toContain("commit");

    const remember = async (record: string, content: string) => {
      const result = await rpc.request("remember", { episode: { schema: "anamnesis.original-message/1", content,
        time: { value: "2026-09-01T00:00:00Z", precision: "second" }, mass: 0.5, properties: {}, origin: { source: root, session: "g7", actor: "fixture", record } },
        source_revision: "v1", expected_previous_revision_key: null });
      expect(result.state).toBe("committed");
      return result as { id: string };
    };
    const a = await remember("a", "ledger reinforcement marker alpha"), b = await remember("b", "ledger reinforcement marker beta");
    const backdated = Date.now() - 30 * 86400000;
    await query("MATCH (e:Element:Episode) WHERE e.id IN $ids SET e.ingested_at=$at", { ids: [a.id, b.id], at: backdated });
    const recallRequest = { query: "ledger reinforcement marker", limit: 10 };
    const first: Recall = await rpc.request("recall", recallRequest);
    expect(first.recall_id).not.toBeNull();
    expect(first.results.map(item => item.id)).toEqual(expect.arrayContaining([a.id, b.id]));
    const before = await cache();

    const commitRequest = { operation_id: uuidv7(), recall_id: first.recall_id, adopted: [a.id] };
    const commitResponse = await rpc.request("commit", commitRequest);
    await record("01-commit-adopt", { recall_request: recallRequest, recall_response: first, request: commitRequest, response: commitResponse });
    expect(commitResponse).toEqual({ ...commitRequest, reward: null, applied: true });
    const after = await cache();
    const hits = (await query("MATCH (h:Hit {kind:'recall_hit'}) RETURN count(h) AS n"))[0]!["n"] as number;
    await record("02-hitcache-before-after", { episodes: { A: a.id, B: b.id }, before, after, recall_hit_count: hits });
    expect(after[a.id]!).toBeGreaterThan(before[a.id] ?? 0);
    expect(hits).toBeGreaterThanOrEqual(1);

    const unknownRequest = { ...commitRequest, operation_id: uuidv7(), recall_id: uuidv7() };
    const invalidRequest = { ...commitRequest, operation_id: uuidv7(), adopted: [uuidv7()] };
    const conflictRequest = { ...commitRequest, reward: 0 };
    const errors = [
      { case: "unknown_recall", request: unknownRequest, response: await failure(rpc.request("commit", unknownRequest)) },
      { case: "invalid_selection", request: invalidRequest, response: await failure(rpc.request("commit", invalidRequest)) },
      { case: "idempotency_conflict", request: conflictRequest, response: await failure(rpc.request("commit", conflictRequest)) },
    ];
    const emptyRequest = { operation_id: uuidv7(), recall_id: first.recall_id };
    await record("03-errors", { cases: errors, observed_only: [{ case: "empty_commit_docs_04_389", request: emptyRequest, response: await Promise.resolve().then(() => rpc.request("commit", emptyRequest)).then(() => null, (error: Error) => ({ thrown_client_side: error.name, message: error.message })) }] });
    expect(errors.map(error => error.response?.data_code)).toEqual(["unknown_recall", "invalid_selection", "idempotency_conflict"]);

    const silent: Client = await RpcClient.connect(`${root}/anamnesis.sock`, token, "receipt"); clients.push(silent);
    const quietBefore = await cache();
    const silentRecall: Recall = await silent.request("recall", recallRequest);
    const quietAfter = await cache();
    await record("04-no-commit-noop", { recall_request: recallRequest, recall_id: silentRecall.recall_id, before: quietBefore, after: quietAfter });
    expect(silentRecall.recall_id).not.toBeNull();
    expect(quietAfter).toEqual(quietBefore);

    const last: Recall = await rpc.request("recall", recallRequest);
    const mass = (result: Recall, id: string) => result.results.find(item => item.id === id)?.mass;
    await record("05-recall-mass-pair", { recall_request: recallRequest, ingested_at_backdated_to: backdated,
      before_commit: { A: mass(first, a.id), B: mass(first, b.id) }, after_commit: { A: mass(last, a.id), B: mass(last, b.id) }, response: last });
    expect(mass(last, a.id)!).toBeGreaterThan(mass(last, b.id)!);

    await rpc.request("shutdown", {});
    expect((await exited)[0]).toBe(0);
  } finally {
    if (child.exitCode === null && child.signalCode === null) { child.kill("SIGKILL"); await exited; }
    await Promise.allSettled(clients.map(client => client.close())); lines.close();
    for (const socket of sockets) socket.destroy();
    const closed = once(server, "close", { signal: AbortSignal.timeout(5000) }); server.close(); await closed;
    await driver.close(); await rm(root, { recursive: true });
  }
}, 120000);
