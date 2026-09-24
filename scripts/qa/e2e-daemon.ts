// Daemon-only live E2E (G7 deliverable 5): the foreground daemon must produce Facts, vectors and relation links BY ITSELF.
// Setup mirrors scripts/qa/e2e-real.ts (owned Neo4j container, ssh tunnel to token-hub haiku + Qwen3 embeddings, ops ingest);
// the manual Engine-driven extraction/coverage/embedding/cutover stages are replaced by ONE "workers" stage that only
// observes the daemon: it awaits the daemon's own `workers_idle` stdout transitions and reads `status` on each of them.
// No Engine is constructed here. Every wait is an event with a bounded deadline; nothing polls on a timer.
//   bun run qa:e2e-daemon        (evidence: .omo/evidence/auto-pipeline/g7/e2e/, override with ANAMNESIS_QA_EVIDENCE_DIR)
import assert from "node:assert/strict";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { appendFile, chmod, cp, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { createServer } from "node:net";
import { createInterface } from "node:readline";
import { promisify } from "node:util";
import neo4j, { type Driver } from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { RpcRememberParams, type RpcStatusResult } from "../../packages/protocol/src/rpc.ts";
import { maskSecrets } from "../../packages/backfill/src/secrets.ts";
import { loadProviderConfig } from "../../app/anamnesis/config.ts";
import { RpcClient } from "../../app/anamnesis/client.ts";

const execute = promisify(execFile);
const image = "neo4j@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d";
const sha = (text: string) => createHash("sha256").update(text).digest("hex");
/** The daemon owns every worker; this bounds how long the run waits for its lanes to cover the ingest watermark. */
const WORKERS_DEADLINE_MS = 90 * 60 * 1000;
async function waitForReady(attempt: () => Promise<unknown>, timeoutMs: number, code: string): Promise<void> {
  const signal = AbortSignal.timeout(timeoutMs);
  while (!signal.aborted) { try { await attempt(); return; } catch { await new Promise(resolve => setImmediate(resolve)); } }
  throw new Error(code);
}
async function freePort() {
  const server = createServer();
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const address = server.address(); assert.ok(address && typeof address !== "string");
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

type DaemonLine = Record<string, unknown>;
interface Arrival { line: DaemonLine; at: number }
interface Waiter { resolve(value: Arrival): void; reject(error: Error): void }
interface Cursor { event: string; buffered: Arrival[]; waiters: Waiter[] }
/** Fan-out over the daemon's JSON stdout lines. A cursor buffers its event from the moment of subscription, so an
 * await never misses a transition emitted before it was requested. `next` coalesces the buffer to the newest line:
 * a status read after the latest idle transition also covers every earlier one. */
function lineStream(child: ChildProcess) {
  const counts: Record<string, number> = {}, cursors = new Set<Cursor>();
  let closed = false;
  createInterface({ input: child.stdout! }).on("line", text => {
    let line: DaemonLine;
    try { line = JSON.parse(text) as DaemonLine; } catch { counts["non_json"] = (counts["non_json"] ?? 0) + 1; return; }
    const event = typeof line["event"] === "string" ? line["event"] : "unknown";
    counts[event] = (counts[event] ?? 0) + 1;
    const arrival = { line, at: Date.now() };
    for (const cursor of cursors) {
      if (cursor.event !== event) continue;
      const waiter = cursor.waiters.shift();
      if (waiter) waiter.resolve(arrival); else cursor.buffered.push(arrival);
    }
  });
  child.once("close", () => { closed = true; for (const cursor of cursors) for (const waiter of cursor.waiters.splice(0)) waiter.reject(new Error("daemon_closed")); });
  return {
    counts: () => ({ ...counts }),
    subscribe(event: string) {
      const cursor: Cursor = { event, buffered: [], waiters: [] };
      cursors.add(cursor);
      return {
        next: (signal: AbortSignal, code: string) => new Promise<Arrival>((resolve, reject) => {
          if (cursor.buffered.length) { const latest = cursor.buffered[cursor.buffered.length - 1]!; cursor.buffered.length = 0; resolve(latest); return; }
          if (closed) { reject(new Error("daemon_closed")); return; }
          if (signal.aborted) { reject(new Error(code)); return; }
          const waiter: Waiter = { resolve: value => { signal.removeEventListener("abort", onAbort); resolve(value); },
            reject: error => { signal.removeEventListener("abort", onAbort); reject(error); } };
          const onAbort = () => { const index = cursor.waiters.indexOf(waiter); if (index >= 0) cursor.waiters.splice(index, 1); reject(new Error(code)); };
          signal.addEventListener("abort", onAbort, { once: true });
          cursor.waiters.push(waiter);
        }),
      };
    },
  };
}
type LineStream = ReturnType<typeof lineStream>;

/** Explicit live opt-in. Owns every resource; never discovers or reuses containers. */
export async function runE2eDaemon(evidence = resolve(".omo/evidence/auto-pipeline/g7/e2e")) {
  await mkdir(evidence, { recursive: true });
  const transcript = join(evidence, "e2e-transcript.log");
  let bearer: string | undefined;
  const redact = (text: string) => bearer ? text.replaceAll(bearer, "[REDACTED]") : text;
  await writeFile(transcript, "");
  const log = async (event: string, value: unknown = {}) => {
    const line = redact(JSON.stringify({ at: new Date().toISOString(), event, value }));
    await appendFile(transcript, line + "\n"); console.log(line);
  };
  const started = Date.now(), durations: Record<string, number> = {};
  // Embeddings are forced on: activation needs one vector per Episode and recall must use the vector channel.
  const llmMinIntervalMs = Number(process.env.ANAMNESIS_QA_LLM_MIN_INTERVAL_MS ?? "3000");
  const deviations = [
    "Extraction, coverage, embedding drain and cutover are the daemon's own worker lanes; this script constructs no Engine and drives no pipeline.",
    "verify/status RPC has no Episode/Fact/generation counters; committed ingest receipts and read-only owned-DB snapshots supplement verify health.",
    "ingest-codex-raw Episodes are pre-lineage (digest version 1) and refuse semantic Facts (echo_lineage_unavailable) by D49 contract; only lineage-admitted transcript Episodes can yield Facts.",
  ];
  const summary: Record<string, unknown> = { status: "running", stage: "setup", episodes: 0, facts_active: 0, vectors: 0, relation_links: null,
    llm_min_interval_ms: llmMinIntervalMs, workers_deadline_ms: WORKERS_DEADLINE_MS,
    embedding_channel: "qwen3-embedding-0.6b via llama-server", recall: [], crash_drain: null, model: "claude-haiku-4-5", workers: null, durations, deviations };
  // Bind-mounted roots must live under $HOME (colima/virtiofs shares only the home directory).
  const qaParent = join(process.env["HOME"] ?? "/tmp", ".cache", "anamnesis-qa"); await mkdir(qaParent, { recursive: true, mode: 0o700 });
  const parent = await mkdtemp(join(qaParent, "ana-g7-")), root = join(parent, "runtime"), secretRoot = await mkdtemp(join(qaParent, "ana-g7-key-"));
  const key = join(secretRoot, "provider.json"), owner = `g7-${randomUUID()}`, name = `anamnesis-qa-${owner}`;
  const password = `qa-${randomUUID()}`;
  // The daemon enables its extraction lane from ANAMNESIS_LLM_* (runtime.ts runs loadProviderConfig when ANAMNESIS_LLM_BASE_URL or
  // ANAMNESIS_EMBEDDING_BASE_URL is set); prompt files come from ANAMNESIS_EXTRACTION_PROMPT_FILE / ANAMNESIS_RELATION_PROMPT_FILE
  // (repo defaults unless the operator overrides them, so they are deliberately NOT stripped below).
  const env: NodeJS.ProcessEnv = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_NEO4J_PASSWORD: password,
    ANAMNESIS_NEO4J_USER: "neo4j", ANAMNESIS_NEO4J_DATABASE: "neo4j", ANAMNESIS_LLM_MODEL: "claude-haiku-4-5",
    ANAMNESIS_LLM_DIALECT: "anthropic_messages", ANAMNESIS_LLM_API_KEY_FILE: key, ANAMNESIS_QA_EMBEDDINGS: "1",
    ANAMNESIS_QA_LLM_MIN_INTERVAL_MS: String(llmMinIntervalMs) };
  // Fixture providers (ANAMNESIS_EXTRACTION_CONFIG / ANAMNESIS_EMBEDDING_CONFIG) and foreign runtime identity are dropped.
  for (const field of Object.keys(env)) if (/^ANAMNESIS_(EMBEDDING_|EXTRACTION_CONFIG|RUNTIME_SOCKET|RUNTIME_TOKEN|NEO4J_CONTAINER|QA_OWNER|OBJECTS_ROOT)/.test(field)) delete env[field];
  let tunnel: ChildProcess | undefined, daemon: ChildProcess | undefined, daemonDone: Promise<void> | undefined, lines: LineStream | undefined;
  let stderrBytes = 0;
  let driver: Driver | undefined, client: RpcClient | undefined, containerCreated = false;
  const receipts: string[] = [`# G7 daemon E2E cleanup receipts\nOwner: ${owner}`];
  const command = async (binary: string, args: string[], commandEnv = env) => {
    try { return (await execute(binary, args, { env: commandEnv, timeout: 300000, maxBuffer: 32 * 1024 * 1024 })).stdout; }
    catch (error) {
      const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown";
      await log("command_failed", { binary, operation: args[0], code });
      throw new Error(`command_failed:${binary}:${args[0]}:${code}`); // Never log provider process environments or arbitrary stderr.
    }
  };
  const ops = async (...args: string[]) => command("node", [resolve("dist/anamnesis-ops.mjs"), ...args]);
  const verify = async () => {
    const value = JSON.parse(await ops("verify")); assert.equal(value.ok, true); await log("verify", value); return value;
  };
  const startDaemon = async () => {
    daemon = spawn("node", [resolve("dist/anamnesis-ops.mjs"), "foreground"], { env, stdio: ["ignore", "pipe", "pipe"] });
    daemon.stderr!.on("data", (bytes: Buffer) => { stderrBytes += bytes.length; }); // Counted, never logged.
    lines = lineStream(daemon);
    const listening = lines.subscribe("listening");
    daemonDone = new Promise<void>((resolve, reject) => { daemon!.once("error", reject); daemon!.once("close", () => resolve()); });
    await new Promise<void>((resolve, reject) => {
      listening.next(AbortSignal.timeout(90000), "daemon_readiness_timeout").then(() => resolve(), reject);
      daemonDone!.then(() => reject(new Error("daemon_exited_before_readiness")), reject);
    });
  };
  const connect = async () => RpcClient.connect(join(root, "anamnesis.sock"), (await readFile(join(root, "token"), "utf8")).trim());
  const ready = async () => waitForReady(() => driver!.verifyConnectivity(), 120000, "bolt_readiness_timeout");
  const snapshot = async () => {
    const result = await driver!.executeQuery(`MATCH (s:Meta {key:'extraction_selector'})
      OPTIONAL MATCH (g:ExtractionGeneration {id:s.generation_id})
      CALL () { MATCH (e:Episode) RETURN count(e) AS episodes }
      CALL (s) { MATCH (f:Fact) WHERE f.generation=s.generation_id RETURN count(f) AS facts_active }
      RETURN episodes,facts_active,s.generation_id AS generation_id,s.selector_version AS selector_version,g.body AS generation`);
    return result.records[0]!.toObject();
  };
  const countQuery = async (cypher: string, parameters: Record<string, unknown> = {}) => Number((await driver!.executeQuery(cypher, parameters)).records[0]!.get("count"));
  let count = 0;
  /** Idle criterion: after the daemon's own transition line, a status whose extraction lane is active (cut over) with
   * covered==live at or beyond every remembered Episode, nothing in flight, and an empty embedding outbox. */
  const settled = (status: RpcStatusResult) => {
    const extraction = status.workers.extraction;
    return extraction.state === "active" && extraction.in_flight === 0 && extraction.covered_ingest_seq === extraction.live_ingest_seq
      && extraction.live_ingest_seq >= count && status.workers.embedding.pending === 0;
  };
  try {
    await writeFile(key, "", { mode: 0o600 });
    await command("scp", ["-q", "-o", "BatchMode=yes", "inonono:~/.config/anamnesis/token-hub-haiku.json", key]);
    await chmod(key, 0o600); assert.equal((await stat(key)).mode & 0o777, 0o600);
    const tunnelPort = await freePort(), embedPort = await freePort();
    const control = join(secretRoot, "ssh.sock");
    tunnel = spawn("ssh", ["-N", "-M", "-S", control, "-o", "ForkAfterAuthentication=no", "-o", "ControlPersist=no", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes",
      "-L", `${tunnelPort}:127.0.0.1:19080`, "-L", `${embedPort}:127.0.0.1:18081`, "inonono"], { stdio: "ignore" });
    await waitForReady(() => execute("ssh", ["-S", control, "-O", "check", "inonono"], { timeout: 3000 }).then(() => undefined), 30000, "tunnel_readiness_failed");
    env.ANAMNESIS_LLM_BASE_URL = `http://127.0.0.1:${tunnelPort}`;
    env.ANAMNESIS_EMBEDDING_BASE_URL = `http://127.0.0.1:${embedPort}`; env.ANAMNESIS_EMBEDDING_MODEL = "qwen3-embedding-0.6b"; env.ANAMNESIS_EMBEDDING_DIMENSIONS = "1024";
    await log("resources", { owner, tunnel_pid: tunnel.pid, local_port: tunnelPort, embed_port: embedPort, credential_mode: "0600" });
    // The same loader the daemon runs: fail before spending an hour if its lane configuration is incomplete. The bearer is kept only for redaction.
    const config = await loadProviderConfig(env); bearer = config.llm.apiKey;
    assert.ok(config.llm.baseUrl && config.llm.apiKey, "daemon_llm_config_incomplete"); assert.ok(config.embedding, "daemon_embedding_config_incomplete");
    await log("daemon_config", { model: env.ANAMNESIS_LLM_MODEL, dialect: env.ANAMNESIS_LLM_DIALECT, embedding_model: config.embedding.model, embedding_dimensions: config.embedding.dimensions,
      extraction_prompt_file: env.ANAMNESIS_EXTRACTION_PROMPT_FILE ?? "default", relation_prompt_file: env.ANAMNESIS_RELATION_PROMPT_FILE ?? "default", llm_min_interval_ms: llmMinIntervalMs });
    const port = await freePort(); env.ANAMNESIS_NEO4J_URI = `bolt://127.0.0.1:${port}`;
    // Reserve an explicit port so docker start keeps the same Bolt endpoint.
    await command("docker", ["create", "--name", name, "--label", `anamnesis.qa.owner=${owner}`, "-p", `127.0.0.1:${port}:7687`,
      "-e", `NEO4J_AUTH=neo4j/${password}`, "-e", "NEO4J_server_memory_heap_initial__size=256m", "-e", "NEO4J_server_memory_heap_max__size=512m", "-e", "NEO4J_server_memory_pagecache_size=128m", image]);
    containerCreated = true; await command("docker", ["start", name]);
    driver = neo4j.driver(env.ANAMNESIS_NEO4J_URI, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true, connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
    await ready(); await startDaemon();
    // Subscribed before the first remember: no idle transition emitted during or after ingest can be missed.
    const idle = lines!.subscribe("workers_idle");
    client = await connect();
    // The daemon destroys sockets idle for 30s (daemon.ts socket.setTimeout); the workers stage waits far longer between
    // idle transitions, so every request goes through a connection that is reopened when the previous one was closed.
    const closedTransport = (error: unknown) => error instanceof Error && /RPC connection (is )?closed/.test(error.message);
    const rpc = { request: (async (method, params) => {
      try { return await client!.request(method, params); }
      catch (error) { if (!closedTransport(error)) throw error; await client!.close().catch(() => undefined); client = await connect(); return await client.request(method, params); }
    }) as RpcClient["request"] };
    const initial = await rpc.request("status", {});
    assert.equal(initial.capabilities.extraction, true, "daemon_extraction_capability_missing"); assert.equal(initial.capabilities.embeddings, true, "daemon_embeddings_capability_missing");
    assert.notEqual(initial.workers.extraction.state, "unconfigured", "extraction_lane_unconfigured");
    await log("daemon_ready", { capabilities: initial.capabilities, workers: initial.workers });
    summary.stage = "ingest";
    const ingestStart = Date.now();
    // The first sorted Codex rollout keeps admission inside the existing 256-source cutover bound.
    const source = join(homedir(), ".codex/sessions");
    const copied = join(parent, "codex-copy"); await mkdir(copied);
    const codexFiles = (await readdir(source, { recursive: true })).filter(file => file.endsWith(".jsonl")).sort().slice(0, 1);
    for (const file of codexFiles) { await mkdir(join(copied, file, ".."), { recursive: true }); await cp(join(source, file), join(copied, file)); }
    await log("source_copy", { source, files: codexFiles, destination: copied, source_access: "read_only" });
    await ops("ingest-codex-raw", copied, join(parent, "raw-checkpoint.json"));
    await verify();
    count = Number((await snapshot()).episodes);
    summary.episodes = count;
    const episodeSource = { raw: { source: "codex-session", files: codexFiles, episodes: count }, converted: 0,
      rule: "Sorted Claude JSONL files and physical lines; first user/assistant text block per record, 40-1200 UTF-8 bytes; preserve timestamp and actor; origin.record=file:line; mass=1; stop at 200 total admitted Episodes." };
    summary.episode_source = episodeSource;
    const converted: RpcRememberParams[] = [];
    if (count < 200) {
      deviations.push("One copied Codex rollout yielded fewer than 200 Episodes; deterministic real Claude text-block conversion filled the remainder via ops ingest. One rollout keeps the run inside the current 256-source cutover bound.");
      deviations.push("The first transcript Episode is admitted metadata-free so the crash-drain replay is an exact legacy retry (explicit lineage cannot spool offline).");
      const fallback = process.env.ANAMNESIS_E2E_FALLBACK_PROJECT ?? join(homedir(), ".claude/projects/-Users-ino-Develop-token-hub");
      const fallbackCopy = join(parent, "fallback-copy"); await cp(fallback, fallbackCopy, { recursive: true });
      for (const file of (await readdir(fallbackCopy)).filter(file => file.endsWith(".jsonl")).sort()) {
        let lineNumber = 0;
        for (const line of (await readFile(join(fallbackCopy, file), "utf8")).split("\n")) {
          lineNumber++; if (!line) continue;
          const record = JSON.parse(line) as { type?: string; timestamp?: string; sessionId?: string; message?: { role?: string; content?: string | { type: string; text?: string }[] } };
          const actor = record.message?.role;
          if (actor !== "user" && actor !== "assistant") continue;
          const raw = record.message?.content;
          const content = typeof raw === "string" ? raw : raw?.find(block => block.type === "text")?.text;
          if (!content || content.length < 40 || Buffer.byteLength(content) > 1200 || !record.timestamp || !Number.isFinite(Date.parse(record.timestamp))) continue;
          if (maskSecrets(content).redactions > 0) continue;
          // Explicit lineage admission (D49): only version-2 Episodes are semantic-eligible; metadata-free imports stay pre-lineage and refuse Facts.
          // The first transcript Episode stays metadata-free on purpose: it is the crash-drain replay source, and only an exact
          // legacy retry may spool offline (explicit lineage needs the database; a v2 retry with different lineage fields is a revision_conflict).
          const lineage = converted.length === 0 ? {} : { origin_role: actor, lineage_mode: "direct", parent_recall_ids: [] };
          const params = RpcRememberParams.parse({ episode: { schema: "anamnesis.original-message/1", content,
            time: { value: new Date(record.timestamp).toISOString(), precision: "second" },
            origin: { source: "claude-transcript", session: record.sessionId ?? file, actor, record: `${file}:${lineNumber}` }, mass: 1, properties: {} },
            source_revision: sha(content), expected_previous_revision_key: null, ...lineage });
          converted.push(params);
          if (converted.length + count >= 200) break;
        }
        if (converted.length + count >= 200) break;
      }
      assert.ok(count + converted.length >= 200, "not enough real transcript Episodes");
      const input = join(parent, "episodes.jsonl"); await writeFile(input, converted.map(value => JSON.stringify(value)).join("\n") + "\n", { mode: 0o600 });
      await ops("ingest", input, join(parent, "converted-checkpoint.json"));
      const checkpoint = JSON.parse(await readFile(join(parent, "converted-checkpoint.json"), "utf8"));
      episodeSource.converted = converted.length;
      await log("conversion", { source: fallback, rule: episodeSource.rule, records: converted.length, checkpoint });
    }
    await verify(); count = Number((await snapshot()).episodes); assert.ok(count >= 200 && count <= 256, "episode_count_out_of_bounds");
    summary.episodes = count; durations.ingest_ms = Date.now() - ingestStart;
    // The daemon keeps running: its worker lanes, not this script, own extraction, coverage, embeddings and cutover.
    summary.stage = "workers";
    const workersStart = Date.now(), deadline = AbortSignal.timeout(WORKERS_DEADLINE_MS);
    const trail: { idle_at: string; workers: RpcStatusResult["workers"]; outbox_pending: RpcStatusResult["outbox_pending"] }[] = [];
    let idleEvents = 0, quotaSince: number | undefined;
    // Status is read only after an idle transition (never on a timer). Each read is also the no-op wake that lets a
    // pipeline the relation judge parked as "pending" resume on the daemon's next turn.
    const awaitSettled = async (): Promise<RpcStatusResult> => {
      for (;;) {
        const arrival = await idle.next(deadline, "workers_deadline_exceeded"); idleEvents++;
        const status = await rpc.request("status", {});
        const entry = { idle_at: new Date(arrival.at).toISOString(), workers: status.workers, outbox_pending: status.outbox_pending };
        trail.push(entry); if (trail.length > 64) trail.shift();
        await log("workers_idle", entry);
        if (settled(status)) return status;
        const lane = status.workers.extraction;
        const lastError = lane.state === "active" || lane.state === "catching_up" ? lane.last_error : null;
        // Same guard as e2e-real: a lane stuck on quota across 30 minutes of idle transitions fails the run instead of burning the deadline.
        if (typeof lastError === "string" && lastError.includes("quota")) { quotaSince ??= arrival.at; if (arrival.at - quotaSince > 30 * 60 * 1000) throw new Error("quota_exhausted_over_30_minutes"); }
        else quotaSince = undefined;
      }
    };
    const final = await awaitSettled();
    durations.workers_ms = Date.now() - workersStart;
    const extraction = final.workers.extraction;
    if (extraction.state !== "active") throw new Error("extraction_not_active");
    if (extraction.live_ingest_seq !== count) deviations.push(`live_ingest_seq ${extraction.live_ingest_seq} differs from the Episode count ${count}; covered==live still holds.`);
    const active = await snapshot();
    summary.facts_active = active.facts_active; summary.generation = { id: active.generation_id, selector_version: active.selector_version, body: active.generation };
    // The selector must point at the generation the daemon's scheduler created and cut over by itself.
    assert.equal(active.generation_id, extraction.generation_id, "selector_generation_mismatch");
    assert.ok(Number(active.facts_active) >= 50, "facts_active_below_50");
    const vectors = await countQuery("MATCH (v:EmbeddingVector) RETURN count(v) AS count");
    const quarantined = await countQuery("MATCH (a:EmbeddingAttempt {state:'quarantined'}) RETURN count(a) AS count");
    summary.vectors = vectors; summary.embedding = { vectors, quarantined, lane: final.workers.embedding };
    assert.equal(vectors, count, "vectors_not_equal_episodes");
    // Relation links are the relation judge's verdicts: CONTRASTS/INVALIDATES Fact->Fact links plus duplicate custody operations
    // (a "duplicate" verdict suppresses the Fact and leaves a content-free MaterializationOperation keyed duplicate:<occurrence>).
    const byType = Object.fromEntries((await driver.executeQuery("MATCH (:Fact)-[l]->(:Fact) RETURN type(l) AS type, count(l) AS n")).records.map(row => [String(row.get("type")), Number(row.get("n"))]));
    const contrasts = byType["CONTRASTS"] ?? 0, invalidates = byType["INVALIDATES"] ?? 0;
    const duplicates = await countQuery("MATCH (o:MaterializationOperation) WHERE o.occurrence_key STARTS WITH 'duplicate:' RETURN count(o) AS count");
    const refused = await countQuery("MATCH (o:MaterializationOperation) WHERE o.occurrence_key STARTS WITH 'refused:' RETURN count(o) AS count");
    summary.relation_links = { contrasts, invalidates, duplicates, total: contrasts + invalidates + duplicates, fact_links_by_type: byType, refused_claims: refused };
    summary.extraction_lane = { completed_total: extraction.completed_total, failed_total: extraction.failed_total, last_error: extraction.last_error };
    summary.workers = { status: final.workers, idle_events: idleEvents, trail_tail: trail.slice(-8) };
    await log("workers_settled", { workers: final.workers, facts_active: active.facts_active, vectors, quarantined, relation_links: summary.relation_links, idle_events: idleEvents });
    assert.ok(contrasts + invalidates + duplicates > 0, "relation_links_absent");
    // Evidence for the comparison doc: 20 source Episodes with the Facts the daemon derived from them and their relation links.
    const sampleRows = (await driver.executeQuery(`MATCH (f:Fact {generation:$generation})-[:DERIVED_FROM]->(e:Episode)
      WITH e, collect(DISTINCT f) AS facts ORDER BY e.ingest_seq LIMIT 20
      UNWIND facts AS f
      OPTIONAL MATCH (f)-[l:CONTRASTS|INVALIDATES]-(o:Fact)
      WITH e, f, collect(CASE WHEN l IS NULL THEN null ELSE {role: type(l), direction: CASE WHEN startNode(l) = f THEN 'out' ELSE 'in' END, other_id: o.id, other_text: left(o.content, 200)} END) AS links
      RETURN e.id AS id, e.ingest_seq AS ingest_seq, left(e.content, 300) AS content, collect({id: f.id, text: f.content, properties: properties(f), links: links}) AS facts
      ORDER BY ingest_seq`, { generation: active.generation_id })).records
      .map(row => row.toObject() as { id: string; ingest_seq: number; content: string; facts: { id: string; text: string; properties: Record<string, unknown>; links: unknown[] }[] });
    const samples = sampleRows.map(row => ({ id: row.id, ingest_seq: row.ingest_seq, content: row.content, facts: row.facts.map(fact => ({ id: fact.id, text: fact.text,
      time: Object.fromEntries(Object.entries(fact.properties).filter(([field]) => /time|valid|observed|occur/i.test(field))), links: fact.links })) }));
    await writeFile(join(evidence, "sample-facts.json"), redact(JSON.stringify({ generation: active.generation_id, episodes: samples }, null, 2)) + "\n");
    summary.sample_facts = { file: "sample-facts.json", episodes: samples.length, facts: samples.reduce((total, row) => total + row.facts.length, 0) };
    assert.ok(samples.length > 0, "sample_facts_empty");
    summary.stage = "recall";
    const recallStart = Date.now();
    const facts = await driver.executeQuery("MATCH (f:Fact {generation:$generation}) RETURN f.content AS content ORDER BY f.id LIMIT 64", { generation: active.generation_id });
    const queries = [...new Set(facts.records.map(row => String(row.get("content"))))].slice(0, 5);
    assert.equal(queries.length, 5, "recall_queries_insufficient");
    const recall: { query: string; kinds: string[]; fact_count: number; fact_rank_min: number | null; ppr_used: boolean; channels: string[]; vector_reason: string; result_count: number; top: unknown[]; companions: unknown[] }[] = [];
    summary.recall = recall; summary.fact_rank_base = 1;
    for (const query of queries) {
      await log("recall_request", { query, limit: 20 });
      // Through the daemon's RPC surface, never an in-process Engine.
      const result = await rpc.request("recall", { query, limit: 20, budget: { unit: "utf8_bytes", limit: 65536 } });
      const factResults = result.results.filter(item => item.kind === "Fact");
      const receipt = { query, kinds: result.results.map(item => item.kind), fact_count: factResults.length,
        fact_rank_min: factResults.length ? result.results.findIndex(item => item.kind === "Fact") + 1 : null,
        ppr_used: result.diagnostics.ppr_used, channels: result.diagnostics.channels_used, vector_reason: result.diagnostics.vector_reason, result_count: result.results.length,
        top: result.results.map(item => ({ ...item, content: item.content.slice(0, 200) })),
        companions: result.companions.map(item => ({ ...item, content: item.content.slice(0, 200) })) };
      recall.push(receipt); await log("recall", receipt);
    }
    const factHits = recall.filter(result => result.fact_count > 0).length;
    summary.recall_fact_hits = factHits;
    if (factHits < 3) deviations.push(`derived Fact ranking: Facts outranked by Episodes in ${5 - factHits}/5 queries`);
    durations.recall_ms = Date.now() - recallStart;
    assert.equal(recall.length, 5, "recall_count_not_5");
    assert.ok(recall.some(result => result.kinds.includes("Fact") && result.ppr_used), "recall_no_fact_with_ppr");
    // Every query must run the vector channel over the daemon-produced vectors and use PPR.
    assert.ok(recall.every(result => result.channels.includes("vector") && result.vector_reason === "available" && result.ppr_used), "recall_channels_incomplete");
    summary.stage = "crash_drain";
    const crashStart = Date.now();
    const firstEpisode = String((await driver.executeQuery("MATCH (e:Episode) RETURN e.id AS id ORDER BY e.ingest_seq LIMIT 1")).records[0]!.get("id"));
    const beforeVerify = await verify(), before = await snapshot();
    await command("docker", ["kill", "--signal", "SIGKILL", name]);
    assert.equal((await rpc.request("status", {})).storage, "unavailable");
    await assert.rejects(rpc.request("policy.set", { policy_id: uuidv7(), selector: { episode_id: firstEpisode }, scope: "content" }), { code: "storage_unavailable" });
    // Replay an already admitted delivery while offline: a real durable spool
    // entry must drain idempotently, without changing Episode/generation counts.
    assert.ok(converted[0], "crash replay needs a converted source receipt");
    // converted[0] was admitted metadata-free, so this is an exact legacy retry: the only
    // delivery shape the spool accepts offline, and it drains idempotently.
    assert.equal("origin_role" in converted[0], false, "crash replay source must be metadata-free");
    assert.equal((await rpc.request("remember", converted[0])).state, "spooled");
    assert.ok((await rpc.request("status", {})).spool.pending > 0);
    // Subscribe before restart/wake: the cursor starts now, so no earlier settle line can satisfy it and the next one cannot be missed.
    // Rejection is converted to a handled outcome so an earlier Docker failure cannot leave an unobserved promise.
    const settledLine = lines!.subscribe("drain_settled").next(AbortSignal.timeout(60000), "daemon_settle_timeout").then(arrival => ({ at: arrival.at }), (cause: Error) => ({ cause }));
    await command("docker", ["start", name]); await ready();
    const wakeAt = Date.now();
    const wake_status = await rpc.request("status", {}); await log("crash_wake_status", wake_status);
    const event = await settledLine; if ("cause" in event) throw event.cause;
    const settle_latency_ms = event.at - wakeAt;
    const settled_status = await rpc.request("status", {}); await log("crash_settled_status", { ...settled_status, settle_latency_ms });
    assert.equal(settled_status.storage, "available"); assert.equal(settled_status.spool.pending, 0);
    assert.equal(settled_status.spool.blocked, 0); assert.equal(settled_status.spool.quarantined, 0);
    const afterVerify = await verify(), after = await snapshot(); assert.deepEqual(after, before);
    assert.equal(beforeVerify.status.outbox_pending, 0); assert.equal(afterVerify.status.outbox_pending, 0);
    durations.crash_drain_ms = Date.now() - crashStart;
    summary.crash_drain = { before, after, equal: true, before_verify: beforeVerify, after_verify: afterVerify, wake_status, settled_status, settle_latency_ms, replay: "existing delivery idempotently spooled and drained", refused_code: "storage_unavailable" };
    summary.status = "passed"; summary.stage = "complete";
  } catch (error) {
    summary.status = "failed";
    // Machine messages are safe; arbitrary provider prose is never logged.
    const machineCode = error && typeof error === "object" && "code" in error && typeof error.code === "string" && /^[a-zA-Z0-9_.]+$/.test(error.code) ? error.code : undefined;
    summary.error = error instanceof Error ? /^[a-z0-9_]+$/.test(error.message) ? error.message : error.name : "unknown";
    if (machineCode) summary.error_code = machineCode;
    if (error instanceof Error) summary.error_location = error.stack?.split("\n").find(line => /\/(packages|scripts|app)\//.test(line))?.trim();
    await log("failed", { code: summary.error, error_code: machineCode, stage: summary.stage, location: summary.error_location }); process.exitCode = 1;
  } finally {
    const keep = summary.status === "failed" && process.env.ANAMNESIS_QA_KEEP_ON_FAILURE === "1";
    let keptContainerId: string | null = null, credentialDeleted = false;
    const cleanupErrors: string[] = [];
    const cleanup = async (label: string, action: () => Promise<void>) => { try { await action(); } catch { cleanupErrors.push(label); receipts.push(`FAILED: ${label}`); } };
    await cleanup("client/driver", async () => { await client?.close(); await driver?.close(); });
    await cleanup("daemon", async () => { if (daemon) { daemon.kill("SIGTERM"); await daemonDone; receipts.push("Owned foreground daemon terminated."); } });
    await cleanup("container", async () => {
      if (!containerCreated) return;
      const inspection = JSON.parse(await command("docker", ["inspect", name]))[0]; assert.equal(inspection.Config.Labels["anamnesis.qa.owner"], owner);
      receipts.push(`Volumes: ${JSON.stringify(inspection.Mounts.map((mount: { Name?: string; Destination: string }) => ({ name: mount.Name, destination: mount.Destination })))}`);
      if (keep) { keptContainerId = inspection.Id; receipts.push(`KEPT for post-mortem: ${name} (${inspection.Id}); owner ${owner}`); }
      else receipts.push(`docker rm -f -v: ${(await command("docker", ["rm", "-f", "-v", inspection.Id])).trim()}`);
    });
    await cleanup("tunnel", async () => { if (tunnel) { const closed = new Promise<void>(resolve => tunnel!.once("close", () => resolve())); tunnel.kill("SIGTERM"); if (tunnel.exitCode === null && tunnel.signalCode === null) await closed; receipts.push(`Owned SSH tunnel pid ${tunnel.pid} terminated.`); } });
    await cleanup("credential", async () => { await rm(key, { force: true }); try { await stat(key); throw new Error("key_remains"); } catch (error) { assert.equal((error as NodeJS.ErrnoException).code, "ENOENT"); }
      credentialDeleted = true;
      const result = await execute("ls", [key]).then(() => "unexpectedly exists", error => String(error.stderr)); receipts.push(`Temporary key deleted; ls: ${result.trim()}`); await rm(secretRoot, { recursive: true, force: true }); });
    await cleanup("roots", async () => {
      if (keep) {
        const passwordFile = join(parent, "neo4j-password"); await writeFile(passwordFile, password, { mode: 0o600 });
        await writeFile(join(evidence, "kept-resources.json"), JSON.stringify({ owner, container: containerCreated ? name : null, container_id: keptContainerId,
          root, temp_root: parent, neo4j_uri: env.ANAMNESIS_NEO4J_URI, neo4j_user: "neo4j", neo4j_password_file: passwordFile, credential_deleted: credentialDeleted }, null, 2) + "\n", { mode: 0o600 });
        summary.resources_kept = true; receipts.push(`KEPT runtime/source-copy root: ${parent}; manual owned cleanup required. Credential root removed: ${secretRoot}`);
      } else { await rm(parent, { recursive: true, force: true }); receipts.push(`Removed runtime/source-copy root: ${parent}; secret root: ${secretRoot}`); }
    });
    if (cleanupErrors.length) { summary.cleanup_errors = cleanupErrors; summary.status = "failed"; process.exitCode = 1; }
    durations.total_ms = Date.now() - started;
    summary.daemon_stdout_events = lines?.counts() ?? null; summary.daemon_stderr_bytes = stderrBytes;
    await writeFile(join(evidence, "cleanup-receipts.md"), receipts.join("\n\n") + "\n");
    await writeFile(join(evidence, "e2e-summary.json"), redact(JSON.stringify(summary, null, 2)) + "\n");
    await log("complete", { status: summary.status, episodes: summary.episodes, facts_active: summary.facts_active, vectors: summary.vectors, relation_links: summary.relation_links, durations });
  }
}
if (import.meta.main) await runE2eDaemon(process.env.ANAMNESIS_QA_EVIDENCE_DIR ? resolve(process.env.ANAMNESIS_QA_EVIDENCE_DIR) : undefined);
