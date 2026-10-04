import assert from "node:assert/strict";
import { spawn, execFile } from "node:child_process";
import { once } from "node:events";
import { createServer, type ServerResponse } from "node:http";
import { mkdtemp, readFile, rm, writeFile, mkdir } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createInterface } from "node:readline";
import { promisify } from "node:util";
import neo4j from "neo4j-driver";
import { RpcClient } from "../../app/anamnesis/client.ts";
import type { RpcStatusResult } from "../../packages/protocol/src/rpc.ts";

const deadline = (ms = 90_000) => AbortSignal.timeout(ms);
const run = promisify(execFile);
type Line = { event?: string; source_id?: string; reason?: string };

/** Keep every daemon line from process launch; a subscriber cannot miss an event emitted before its next await. */
function launch(bundle: string, env: NodeJS.ProcessEnv) {
  const child = spawn("node", [bundle], { env, stdio: ["ignore", "pipe", "pipe"] });
  const lines: Line[] = [];
  const output: string[] = [];
  let stderr = "";
  const waiters = new Set<() => void>();
  createInterface({ input: child.stdout! }).on("line", text => {
    output.push(text);
    const line: Line = JSON.parse(text);
    lines.push(line);
    for (const wake of waiters) wake();
  });
  child.stderr!.on("data", (bytes: Buffer) => { stderr += bytes.toString(); });
  const exit = once(child, "close").then(([code, signal]) => ({ code, signal }));
  let cursor = 0;
  const until = async (event: string, predicate: (line: Line) => boolean = () => true, ms = 180_000): Promise<Line> => {
    const signal = deadline(ms);
    for (;;) {
      while (cursor < lines.length) {
        const line = lines[cursor++]!;
        if (line.event === event && predicate(line)) return line;
      }
      assert.equal(child.exitCode, null, `daemon exited while waiting for ${event}: ${stderr}`);
      await Promise.race([
        new Promise<void>((wake, reject) => {
          const done = () => { waiters.delete(done); signal.removeEventListener("abort", abort); wake(); };
          const abort = () => { waiters.delete(done); reject(new Error(`deadline awaiting ${event}: stdout=${output.join("\n")} stderr=${stderr}`)); };
          waiters.add(done);
          signal.addEventListener("abort", abort, { once: true });
          if (lines.length > cursor) done();
        }),
        exit.then(result => { throw new Error(`daemon exited awaiting ${event}: ${JSON.stringify(result)} ${stderr}`); }),
      ]);
    }
  };
  const stop = async () => {
    if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    return await exit;
  };
  return { child, lines, until, stop, exit, stderr: () => stderr };
}

export async function runG3ExtractionCrash(uri: string, password: string, evidenceRoot: string) {
  const workspace = resolve(new URL("../../", import.meta.url).pathname);
  const root = await mkdtemp("/tmp/ana-g3-crash-");
  const build = await mkdtemp("/tmp/ana-g3-bundle-");
  const bundle = join(build, "daemon.mjs");
  const key = join(root, "provider.json");
  const model = "g3-crash-fixture";
  let held: ServerResponse | undefined;
  let gateClaim = true;
  let received!: () => void;
  const requestReceived = new Promise<void>(resolveReceived => { received = resolveReceived; });
  let interruptedText = "";
  const server = createServer(async (request, response) => {
    const chunks: Buffer[] = [];
    for await (const chunk of request) chunks.push(chunk as Buffer);
    const body: unknown = JSON.parse(Buffer.concat(chunks).toString("utf8"));
    assert.ok(body && typeof body === "object" && "messages" in body && Array.isArray(body.messages));
    const user = body.messages.find((message: { role?: string }) => message.role === "user");
    const input: { task: string; text: string; claim_context?: { body_digest: string; claims: { evidence: { text: string } }[] };
      relation_context?: { body_digest: string; candidates: { id: string }[] } } = JSON.parse(user.content);
    if (input.task === "claim" && input.text === interruptedText && gateClaim) {
      gateClaim = false;
      held = response;
      received();
      return;
    }
    const output = input.task === "claim"
      ? { task: "claim", claims: [{ text: input.text, evidence: input.text, confidence: 0.9,
        entities: [{ mention: "grounded", normalized_name: "grounded", entity_kind: "concept" }] }], language: "en", modality: "text" }
      : input.task === "judge_claims"
        ? { task: "judge_claims", claim_body_digest: input.claim_context?.body_digest,
          decisions: input.claim_context?.claims.map((claim, claim_index) => ({ claim_index, disposition: "retain", evidence: claim.evidence.text, confidence: 0.9 })),
          language: "en", modality: "text" }
        : { task: "judge_relations", relation_context_digest: input.relation_context?.body_digest,
          judgements: input.relation_context?.candidates.map(candidate => ({ candidate_id: candidate.id, relation: "unrelated", confidence: 0.9, reason: "fixture" })),
          language: "en", modality: "text" };
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ model, choices: [{ message: { content: JSON.stringify(output) } }] }));
  });
  let first: ReturnType<typeof launch> | undefined, second: ReturnType<typeof launch> | undefined;
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  try {
    const databaseReady = deadline(180_000);
    while (!databaseReady.aborted) {
      try {
        await driver.verifyConnectivity();
        await driver.executeQuery("RETURN 1 AS ready");
        break;
      } catch {
        await Promise.race([once(driver, "error").catch(() => undefined), Promise.resolve()]);
      }
    }
    databaseReady.throwIfAborted();
    await run(process.execPath, ["build", "app/anamnesis/main.ts", "--target=node", "--outfile", bundle], { cwd: workspace });
    await writeFile(key, JSON.stringify({ bearer: "g3-fixture-key" }), { mode: 0o600 });
    const listening = once(server, "listening", { signal: deadline() });
    server.listen(0, "127.0.0.1");
    await listening;
    const address = server.address();
    assert.ok(address && typeof address !== "string");
    const env: NodeJS.ProcessEnv = { ...process.env };
    for (const name of Object.keys(env)) if (/^ANAMNESIS_(LLM_|EMBEDDING_|EXTRACTION_|LISTEN)/.test(name)) delete env[name];
    Object.assign(env, { ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: "g3-installation-token",
      ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_USER: "neo4j", ANAMNESIS_NEO4J_PASSWORD: password,
      ANAMNESIS_LLM_BASE_URL: `http://127.0.0.1:${address.port}`, ANAMNESIS_LLM_API_KEY_FILE: key,
      ANAMNESIS_LLM_DIALECT: "openai_chat", ANAMNESIS_LLM_MODEL: model, ANAMNESIS_EXTRACTION_MAX_IN_FLIGHT: "1",
      ANAMNESIS_EXTRACTION_PROMPT_FILE: join(workspace, "app/anamnesis/prompts/extract-claims.v2.md"),
      ANAMNESIS_RELATION_PROMPT_FILE: join(workspace, "app/anamnesis/prompts/judge-relations.v1.md") });
    // One connection per request: the daemon destroys an RPC socket idle for 30 s (daemon.ts socket.setTimeout) and the
    // waits below (lease expiry is 90 s) outlive that.
    const withClient = async <T>(work: (client: RpcClient) => Promise<T>): Promise<T> => {
      const client = await RpcClient.connect(join(root, "anamnesis.sock"), "g3-installation-token");
      try { return await work(client); } finally { await client.close(); }
    };
    const statusOnce = () => withClient(client => client.request("status", {}));
    first = launch(bundle, env);
    await first.until("listening");
    assert.equal((await statusOnce()).capabilities.extraction, true);
    const ids: string[] = [];
    const remember = async (n: number) => {
      const content = `G3 crash fixture Episode ${n} records a distinct grounded memory.`;
      const result = await withClient(client => client.request("remember", { episode: { schema: "anamnesis.original-message/1", content,
        time: { value: `2026-09-01T00:00:0${n}Z`, precision: "second" }, origin: { source: root, session: "crash", actor: "user", record: String(n) },
        mass: 1, properties: {} }, source_revision: "v1", expected_previous_revision_key: null,
        origin_role: "user", lineage_mode: "direct", parent_recall_ids: [] }));
      assert.equal(result.state, "committed");
      if (result.state === "committed") ids.push(result.id);
    };
    await remember(1);
    await remember(2);
    const covered = async (generation: string) =>
      Number((await driver.executeQuery("MATCH (c:ExtractionCoverage {generation_id:$generation}) RETURN min(c.covered_ingest_seq) AS covered", { generation })).records[0]?.get("covered") ?? 0);
    let status: RpcStatusResult;
    for (;;) {
      status = await statusOnce();
      const lane = status.workers.extraction;
      if (lane.state === "active" && lane.covered_ingest_seq === 2 && lane.in_flight === 0) break;
      await first.until("workers_idle");
    }
    interruptedText = "G3 crash fixture Episode 3 records a distinct grounded memory.";
    await remember(3);
    await remember(4);
    await Promise.race([requestReceived, first.exit.then(result => { throw new Error(`daemon exited before provider request: ${JSON.stringify(result)} ${first?.stderr()}`); }),
      once(deadline(), "abort").then(() => { throw new Error("claim request deadline"); })]);
    const interruptedSourceId = ids[2];
    assert.ok(interruptedSourceId);
    const leased = first.lines.some(line => line.event === "extraction.task.leased" && line.source_id === interruptedSourceId);
    assert.ok(leased, "missing leased audit line for interrupted Episode");
    assert.equal((await first.stop()).signal, "SIGKILL");
    const journal: unknown = JSON.parse(await readFile(join(root, "extraction-state.json"), "utf8"));
    assert.ok(journal && typeof journal === "object" && "pipelines" in journal && journal.pipelines && typeof journal.pipelines === "object");
    const entries = Object.values(journal.pipelines);
    assert.ok(entries.some(entry => entry && typeof entry === "object" && "source_id" in entry && entry.source_id === interruptedSourceId &&
      "claim" in entry && entry.claim && typeof entry.claim === "object" && "state" in entry.claim && entry.claim.state === "leased"),
    "interrupted Episode must remain leased in the on-disk journal");
    const coveredAfterKill = await covered(status.workers.extraction.state === "active" ? status.workers.extraction.generation_id : "");
    assert.equal(coveredAfterKill, 2);
    assert.ok(held);
    held.destroy();
    held = undefined;
    second = launch(bundle, env);
    await second.until("listening");
    for (;;) {
      status = await statusOnce();
      const lane = status.workers.extraction;
      if (lane.state === "active" && lane.live_ingest_seq === 4 && lane.covered_ingest_seq === 4 && lane.in_flight === 0) break;
      await second.until("workers_idle");
    }
    const settled = second.lines.some(line => line.event === "extraction.task.settled" && line.source_id === interruptedSourceId &&
      (line.reason === "worker_lost" || line.reason === "expired"));
    assert.ok(settled, `missing settled audit line: ${JSON.stringify(second.lines)}`);
    assert.ok(second.lines.some(line => line.event === "extraction.attempt.recorded" && line.source_id === interruptedSourceId),
      "missing attempt audit line for resumed Episode");
    const rows = (await driver.executeQuery("MATCH (f:Fact) RETURN count(f) AS facts,count(DISTINCT f.digest) AS distinct_digests")).records[0];
    assert.ok(rows);
    const facts = Number(rows.get("facts")), distinctDigests = Number(rows.get("distinct_digests"));
    assert.equal(facts, 4);
    assert.equal(distinctDigests, facts);
    const coveredFinal = await covered(status.workers.extraction.state === "active" ? status.workers.extraction.generation_id : "");
    assert.equal(coveredFinal, 4);
    assert.ok(coveredFinal >= coveredAfterKill);
    const labels = (await driver.executeQuery("CALL db.labels() YIELD label RETURN collect(label) AS labels")).records[0]?.get("labels");
    assert.ok(Array.isArray(labels));
    for (const label of ["ModelTask", "ExtractionAttempt", "ExtractionPipeline", "ExtractionJudgeInput", "ExtractionDisposition"])
      assert.equal(labels.includes(label), false, `${label} persisted in Neo4j`);
    const result = { episodes: ids.length, facts, distinct_digests: distinctDigests, covered_after_kill: coveredAfterKill,
      covered_final: coveredFinal, interrupted_source_id: interruptedSourceId, leased_line_seen: leased, settled_line_seen: settled };
    await mkdir(evidenceRoot, { recursive: true });
    await writeFile(join(evidenceRoot, "c002.json"), JSON.stringify(result, null, 2) + "\n");
    return result;
  } finally {
    await first?.stop();
    await second?.stop();
    server.closeAllConnections();
    if (server.listening) await new Promise<void>((done, reject) => server.close(error => error ? reject(error) : done()));
    await driver.close();
    await rm(root, { recursive: true, force: true });
    await rm(build, { recursive: true, force: true });
  }
}
