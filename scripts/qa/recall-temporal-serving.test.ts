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

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OUT = process.env["ANAMNESIS_G6_SURFACE_OUT"]
  ?? "/Users/ino/Develop/anamnesis/.omo/evidence/ulw/01a11b8c-9f0b-71f3-ac0c-cb1048614a4e/G001-g6-recall-serving-fact-assembly-drop/surface";
const profile = { model: "g6-wiring-fixture-v1", model_incarnation: "a".repeat(64), dimensions: 3,
  document_prefix: "document: ", query_prefix: "query: ", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 };
const extraction = { model: "g6-extraction-fixture", model_incarnation: "b".repeat(64) };
type Item = { id: string; kind: string; time: { value: string }; rank?: number };
type Recall = { results: Item[]; companions: Item[] };

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

test("time window, chronological order, known_by and the T bound on the real daemon over UDS", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const { RpcClient } = await import(`${ROOT}dist/anamnesis-client.mjs`);
  const root = await mkdtemp("/tmp/g6-recall-serving-"), token = randomUUID();
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
  let client: { request(method: string, params: unknown): Promise<any>; close(): Promise<void> } | undefined;
  try {
    await query("MATCH (n) DETACH DELETE n");
    const ready = daemonEvent("listening");
    await Promise.race([ready, exited.then(([code]) => { throw Error(`daemon exit ${code}`); })]);
    client = await RpcClient.connect(`${root}/anamnesis.sock`, token);
    const rpc = client!;
    const remember = async (record: string, content: string, value: string) => {
      const result = await rpc.request("remember", { episode: { schema: "anamnesis.original-message/1", content,
        time: { value, precision: "second" }, mass: 0, properties: {}, origin: { source: root, session: "g6", actor: "fixture", record } },
        source_revision: "v1", expected_previous_revision_key: null, origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] });
      expect(result.state).toBe("committed");
      return result as { id: string };
    };
    const factFor = async (source: { id: string }) => {
      const read = async () => (await query("MATCH (f:Element:Fact)-[:DERIVED_FROM]->(:Episode {id:$id}) RETURN f.id AS id ORDER BY id", { id: source.id }))[0]?.["id"] as string | undefined;
      for (let id = await read(); ; id = await read()) { if (id) return id; await daemonEvent("workers_idle"); }
    };
    const t1 = Date.parse("2026-09-01T00:00:00Z"), t2 = Date.parse("2026-09-02T00:00:00Z"), t3 = Date.parse("2026-09-03T00:00:00Z");
    const e1 = await remember("one", "chronicle alpha marker", "2026-09-01T00:00:00Z");
    const f1 = await factFor(e1);
    const e2 = await remember("two", "chronicle beta marker", "2026-09-02T00:00:00Z");
    const e3 = await remember("three", "chronicle gamma marker", "2026-09-03T00:00:00Z");
    const facts = [f1, await factFor(e2), await factFor(e3)];
    const [generation] = await query("MATCH (g:ExtractionGeneration) RETURN g.id AS id,g.body AS body");
    if (!generation) throw new Error("extraction generation missing");
    const active = { ...JSON.parse(String(generation["body"])), state: "active" };
    const body = JSON.stringify(Object.fromEntries(Object.entries(active).sort(([left], [right]) => left < right ? -1 : left > right ? 1 : 0)));
    await query("MATCH (g:ExtractionGeneration {id:$id}),(s:Meta {key:'extraction_selector'}) SET g.state='active',g.body=$body,s.generation_id=$id,s.selector_version=1",
      { id: generation["id"], body });
    const ingested = new Map((await query("MATCH (e:Element) WHERE e.id IN $ids RETURN e.id AS id,e.ingested_at AS at",
      { ids: [e1.id, e2.id, e3.id, ...facts] })).map(row => [String(row["id"]), Number(row["at"])]));
    const k1 = ingested.get(e1.id)!;
    const base = { query: "chronicle", limit: 10, budget: { unit: "utf8_bytes", limit: 65536 } };
    const record = async (name: string, request: unknown, response: unknown) =>
      writeFile(`${OUT}/${name}.json`, JSON.stringify({ request, response }, null, 2) + "\n");
    const all = (result: Recall) => [...result.results, ...result.companions];

    const windowRequest = { ...base, time: { from: t2, to: t3 } };
    const windowed: Recall = await rpc.request("recall", windowRequest);
    await record("01-time-window", windowRequest, windowed);
    expect(windowed.results.length).toBeGreaterThan(0);
    for (const item of all(windowed)) { const at = Date.parse(item.time.value); expect(at >= t2 && at <= t3).toBe(true); }
    expect(windowed.results.map(item => item.id)).not.toContain(e1.id);
    expect(windowed.results.map(item => item.id)).not.toContain(f1);

    const chronologicalRequest = { ...base, order: "chronological" };
    const chronological: Recall = await rpc.request("recall", chronologicalRequest);
    await record("02-chronological", chronologicalRequest, chronological);
    expect(chronological.results.length).toBeGreaterThan(1);
    const times = chronological.results.map(item => Date.parse(item.time.value));
    expect(times).toEqual([...times].sort((a, b) => a - b));
    expect(chronological.results.map(item => item.rank)).toEqual(chronological.results.map((_, rank) => rank));

    const knownRequest = { ...base, known_by: k1 };
    const known: Recall = await rpc.request("recall", knownRequest);
    await record("03-known-by", knownRequest, { ...known, ingested_at: Object.fromEntries(ingested), known_by: k1 });
    expect(known.results.map(item => item.id)).toEqual([e1.id]);
    for (const item of all(known)) expect(ingested.get(item.id) ?? (await query("MATCH (e:Element {id:$id}) RETURN e.ingested_at AS at", { id: item.id }))[0]!["at"]).toBeLessThanOrEqual(k1);

    const futureRequest = { ...base, time: { to: Date.now() + 86400000 } };
    const failure = await rpc.request("recall", futureRequest).then(() => null, (error: { code: string; message: string }) => error);
    await record("04-future-to", futureRequest, { error: failure && { code: failure.code, message: failure.message } });
    expect(failure?.code).toBe("invalid_params");

    await rpc.request("shutdown", {});
    expect((await exited)[0]).toBe(0);
  } finally {
    if (child.exitCode === null && child.signalCode === null) { child.kill("SIGKILL"); await exited; }
    await client?.close(); lines.close();
    for (const socket of sockets) socket.destroy();
    const closed = once(server, "close", { signal: AbortSignal.timeout(5000) }); server.close(); await closed;
    await driver.close(); await rm(root, { recursive: true });
  }
}, 120000);
