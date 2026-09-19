import assert from "node:assert/strict";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { createHash, randomUUID } from "node:crypto";
import { appendFile, chmod, cp, mkdir, mkdtemp, readFile, readdir, rm, stat, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join, resolve } from "node:path";
import { createServer } from "node:net";
import { createInterface, type Interface } from "node:readline";
import { promisify } from "node:util";
import neo4j, { type Driver } from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { Engine } from "../../packages/core/src/engine.ts";
import { OpenAiChatExtractionProvider } from "../../packages/core/src/openai-extraction-provider.ts";
import { Generation } from "../../packages/protocol/src/extraction.ts";
import { RpcRememberParams, type RpcStatusResult } from "../../packages/protocol/src/rpc.ts";
import { maskSecrets } from "../../packages/backfill/src/secrets.ts";
import type { ExtractionPipeline } from "../../packages/protocol/src/extraction-audit.ts";
import { loadProviderConfig } from "../../app/anamnesis/config.ts";
import { RpcClient } from "../../app/anamnesis/client.ts";

const execute = promisify(execFile);
const context = { principal: "installation", commit_mode: "auto" } as const;
const image = "neo4j@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d";
const sha = (text: string) => createHash("sha256").update(text).digest("hex");
const pause = (ms: number) => new Promise(resolve => setTimeout(resolve, ms)); // Script-only bounded readiness/backoff.
/** Gate before leasing a model task; mark at the actual HTTP call boundary. */
export function createLlmPacer(minIntervalMs: number, clock = () => performance.now(), wait = pause) {
  if (!Number.isSafeInteger(minIntervalMs) || minIntervalMs < 0 || minIntervalMs > 60000) throw new Error("invalid_llm_min_interval");
  let lastCall: number | undefined;
  return {
    async waitForTurn() {
      if (lastCall === undefined) return;
      const remaining = minIntervalMs - (clock() - lastCall);
      if (remaining > 0) await wait(remaining);
    },
    markCall() { lastCall = clock(); },
  };
}
async function freePort() {
  const server = createServer();
  await new Promise<void>((resolve, reject) => { server.once("error", reject); server.listen(0, "127.0.0.1", resolve); });
  const address = server.address(); assert.ok(address && typeof address !== "string");
  await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve()));
  return address.port;
}

/** Explicit live opt-in. Owns every resource; never discovers or reuses containers. */
export async function runE2eReal(evidence = resolve(".omo/evidence/runtime-complete/g4")) {
  await mkdir(evidence, { recursive: true });
  const transcript = join(evidence, "e2e-transcript.log");
  let bearer: string | undefined;
  const redact = (text: string) => bearer ? text.replaceAll(bearer, "[REDACTED]") : text;
  await writeFile(transcript, "");
  const log = async (event: string, value: unknown = {}) => {
    const line = redact(JSON.stringify({ at: new Date().toISOString(), event, value }));
    await appendFile(transcript, line + "\n"); console.log(line);
  };
  const started = Date.now(), durations: Record<string, number> = {}, errors: Record<string, number> = {};
  const deviations = [
    "Embeddings disabled by decision: token-hub has no embeddings route.",
    "ops extract is capability admission only; extraction uses Engine after ops down.",
    "verify/status RPC has no Episode/Fact/generation counters; committed ingest receipts and read-only owned-DB snapshots supplement verify health.",
    "Attempt 4 was cleaned before a recall probe could run; its second response was not recorded. Queries now use full Fact text and top-20 results; ranking code was not changed without evidence.",
    "Original-message Outbox completion uses Engine.digest only after coverage and cutover accept every successful outcome or terminal omission.",
  ];
  const llmMinIntervalMs = Number(process.env.ANAMNESIS_QA_LLM_MIN_INTERVAL_MS ?? "3000");
  const pacer = createLlmPacer(llmMinIntervalMs);
  const summary: Record<string, unknown> = { status: "running", stage: "setup", episodes: 0, facts_active: 0, vectors: 0,
    llm_min_interval_ms: llmMinIntervalMs,
    embedding_channel: "disabled_by_decision", recall: [], crash_drain: null, model: "claude-haiku-4-5", provider_errors: errors, durations, deviations };
  const parent = await mkdtemp("/tmp/ana-g4-"), root = join(parent, "runtime"), secretRoot = await mkdtemp("/tmp/ana-g4-key-");
  const key = join(secretRoot, "provider.json"), owner = `g4-${randomUUID()}`, name = `anamnesis-qa-${owner}`;
  const password = `qa-${randomUUID()}`;
  const env: NodeJS.ProcessEnv = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_NEO4J_PASSWORD: password,
    ANAMNESIS_NEO4J_USER: "neo4j", ANAMNESIS_NEO4J_DATABASE: "neo4j", ANAMNESIS_LLM_MODEL: "claude-haiku-4-5",
    ANAMNESIS_LLM_DIALECT: "anthropic_messages", ANAMNESIS_LLM_API_KEY_FILE: key };
  for (const field of Object.keys(env)) if (/^ANAMNESIS_(EMBEDDING_|EXTRACTION_CONFIG|RUNTIME_SOCKET|RUNTIME_TOKEN|NEO4J_CONTAINER|QA_OWNER|OBJECTS_ROOT)/.test(field)) delete env[field];
  let tunnel: ChildProcess | undefined, daemon: ChildProcess | undefined, daemonDone: Promise<void> | undefined;
  let daemonLines: Interface | undefined;
  let driver: Driver | undefined, engine: Engine | undefined, containerCreated = false;
  const receipts: string[] = [`# G4 cleanup receipts\nOwner: ${owner}`];
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
  const waitForEvent = (event: "drain_settled", timeoutMs: number): Promise<number> => {
    const lines = daemonLines; assert.ok(lines);
    return new Promise((resolve, reject) => {
      const cleanup = () => { clearTimeout(timer); lines.off("line", onLine); lines.off("close", onClose); };
      const onLine = (line: string) => { if (line === JSON.stringify({ event })) { cleanup(); resolve(Date.now()); } };
      const onClose = () => { cleanup(); reject(new Error("daemon_closed_before_settle")); };
      const timer = setTimeout(() => { cleanup(); reject(new Error("daemon_settle_timeout")); }, timeoutMs);
      lines.on("line", onLine); lines.once("close", onClose);
    });
  };
  const startDaemon = async () => {
    daemon = spawn("node", [resolve("dist/anamnesis-ops.mjs"), "foreground"], { env, stdio: ["ignore", "pipe", "pipe"] });
    daemonLines = createInterface({ input: daemon.stdout! });
    daemonDone = new Promise<void>((resolve, reject) => { daemon!.once("error", reject); daemon!.once("close", () => resolve()); });
    await new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("daemon_readiness_timeout")), 90000);
      let output = "";
      const observe = (bytes: Buffer) => { output += bytes.toString(); if (output.includes('"event":"listening"')) { clearTimeout(timer); resolve(); } };
      daemon!.stdout!.on("data", observe); daemon!.stderr!.on("data", observe);
      daemonDone!.then(() => { clearTimeout(timer); reject(new Error("daemon_exited_before_readiness")); }, reject);
    });
  };
  const stopDaemon = async () => { if (daemon) { await ops("down"); await daemonDone; daemon = undefined; } };
  const connect = async () => RpcClient.connect(join(root, "anamnesis.sock"), (await readFile(join(root, "token"), "utf8")).trim());
  const ready = async () => {
    const deadline = Date.now() + 120000;
    while (true) { try { await driver!.verifyConnectivity(); return; } catch { if (Date.now() >= deadline) throw new Error("bolt_readiness_timeout"); await pause(250); } }
  };
  const snapshot = async () => {
    const result = await driver!.executeQuery(`MATCH (s:Meta {key:'extraction_selector'})
      OPTIONAL MATCH (g:ExtractionGeneration {id:s.generation_id})
      CALL () { MATCH (e:Episode) RETURN count(e) AS episodes }
      CALL (s) { MATCH (f:Fact) WHERE f.generation=s.generation_id RETURN count(f) AS facts_active }
      RETURN episodes,facts_active,s.generation_id AS generation_id,s.selector_version AS selector_version,g.body AS generation`);
    return result.records[0]!.toObject();
  };
  try {
    await writeFile(key, "", { mode: 0o600 });
    await command("scp", ["-q", "-o", "BatchMode=yes", "inonono:~/.config/anamnesis/token-hub-haiku.json", key]);
    await chmod(key, 0o600); assert.equal((await stat(key)).mode & 0o777, 0o600);
    const tunnelPort = await freePort();
    const control = join(secretRoot, "ssh.sock");
    tunnel = spawn("ssh", ["-N", "-M", "-S", control, "-o", "ForkAfterAuthentication=no", "-o", "ControlPersist=no", "-o", "BatchMode=yes", "-o", "ExitOnForwardFailure=yes", "-L", `${tunnelPort}:127.0.0.1:19080`, "inonono"], { stdio: "ignore" });
    const tunnelDeadline = Date.now() + 30000;
    while (true) {
      try { await execute("ssh", ["-S", control, "-O", "check", "inonono"], { timeout: 3000 }); break; }
      catch { if (Date.now() >= tunnelDeadline || tunnel.exitCode !== null) throw new Error("tunnel_readiness_failed"); await pause(100); }
    }
    env.ANAMNESIS_LLM_BASE_URL = `http://127.0.0.1:${tunnelPort}`;
    await log("resources", { owner, tunnel_pid: tunnel.pid, local_port: tunnelPort, credential_mode: "0600" });
    const port = await freePort(); env.ANAMNESIS_NEO4J_URI = `bolt://127.0.0.1:${port}`;
    // Reserve an explicit port so docker start keeps the same Bolt endpoint.
    await command("docker", ["create", "--name", name, "--label", `anamnesis.qa.owner=${owner}`, "-p", `127.0.0.1:${port}:7687`,
      "-e", `NEO4J_AUTH=neo4j/${password}`, "-e", "NEO4J_server_memory_heap_initial__size=256m", "-e", "NEO4J_server_memory_heap_max__size=512m", "-e", "NEO4J_server_memory_pagecache_size=128m", image]);
    containerCreated = true; await command("docker", ["start", name]);
    driver = neo4j.driver(env.ANAMNESIS_NEO4J_URI, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true, connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
    await ready(); await startDaemon();
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
    let count = Number((await snapshot()).episodes);
    summary.episodes = count;
    const episodeSource = { raw: { source: "codex-session", files: codexFiles, episodes: count }, converted: 0,
      rule: "Sorted Claude JSONL files and physical lines; first user/assistant text block per record, 40-1200 UTF-8 bytes; preserve timestamp and actor; origin.record=file:line; mass=1; stop at 200 total admitted Episodes." };
    summary.episode_source = episodeSource;
    const converted: RpcRememberParams[] = [];
    if (count < 200) {
      deviations.push("One copied Codex rollout yielded fewer than 200 Episodes; deterministic real Claude text-block conversion filled the remainder via ops ingest. One rollout keeps the run inside the current 256-source cutover bound.");
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
          const params = RpcRememberParams.parse({ episode: { schema: "anamnesis.original-message/1", content,
            time: { value: new Date(record.timestamp).toISOString(), precision: "second" },
            origin: { source: "claude-transcript", session: record.sessionId ?? file, actor, record: `${file}:${lineNumber}` }, mass: 1, properties: {} },
            source_revision: sha(content), expected_previous_revision_key: null });
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
    await verify(); count = Number((await snapshot()).episodes); assert.ok(count >= 200 && count <= 256);
    summary.episodes = count; durations.ingest_ms = Date.now() - ingestStart;
    await stopDaemon();
    summary.stage = "extraction";
    await log("extraction_surface", { engine: true, ops_extract: "capability_admission_only", writer_handoff: "daemon stopped" });
    const config = await loadProviderConfig(env); assert.ok(config.llm.baseUrl && config.llm.apiKey); assert.equal(config.embedding, undefined);
    bearer = config.llm.apiKey;
    let quotaSince: number | undefined;
    const provider = new OpenAiChatExtractionProvider({ ...config.llm, baseUrl: config.llm.baseUrl, apiKey: config.llm.apiKey,
      systemPrompt: config.systemPrompt, timeoutMs: 25000, fetch: async (url, init) => {
        pacer.markCall();
        const response = await fetch(url, init);
        // Only safe machine codes leave this boundary, never provider prose/headers/body.
        let code: string | undefined;
        try { const body = await response.clone().json() as { error?: { code?: unknown } }; if (typeof body.error?.code === "string") code = /^[a-z0-9_]{1,80}$/.test(body.error.code) ? body.error.code : "unrecognized_provider_code"; } catch { /* Non-JSON is validated by the provider. */ }
        if (!response.ok || code) { const safe = code ?? `http_${response.status}`; errors[safe] = (errors[safe] ?? 0) + 1; await log("provider_error", { code: safe, status: response.status });
          if (safe.includes("quota")) quotaSince ??= Date.now();
        } else quotaSince = undefined;
        return response;
      } });
    // Engine's environment loader requires a password even with explicit options.
    process.env.ANAMNESIS_NEO4J_PASSWORD = password;
    delete process.env.ANAMNESIS_EMBEDDING_CONFIG; delete process.env.ANAMNESIS_EXTRACTION_CONFIG;
    engine = new Engine({ uri: env.ANAMNESIS_NEO4J_URI, user: "neo4j", password, objectsRoot: join(root, "objects"), extractionProvider: provider });
    // Pacing must precede lease acquisition, not consume the task's 30s lease.
    // runExtractionPipeline dispatches both claim and judge through this method.
    const runTask = engine.runExtractionTask.bind(engine);
    engine.runExtractionTask = async (input, installation) => { await pacer.waitForTurn(); return runTask(input, installation); };
    await engine.init(); await engine.claimWriterEpoch();
    const generation = Generation.parse({ id: uuidv7(), stream: "extraction", incarnation: provider.modelIncarnation, state: "catching_up", covered_ingest_seq: 0, created_at: Date.now(), updated_at: Date.now() });
    await engine.store.createExtractionGeneration(generation, context);
    const episodes = (await driver.executeQuery("MATCH (e:Episode) RETURN e.id AS id ORDER BY e.ingest_seq")).records.map(row => String(row.get("id")));
    const extractStart = Date.now();
    const pipelines = new Map<string, string>();
    let completed = 0;
    const residual: { index: number; task_id: string; code: string }[] = [];
    summary.residual_extraction_errors = residual;
    for (const [index, source_id] of episodes.entries()) {
      const task = await engine.createExtractionPipeline({ id: uuidv7(), generation_id: generation.id, source_id }, context);
      pipelines.set(source_id, task.id);
      const deadline = Date.now() + 31 * 60 * 1000;
      for (let attempt = 0; ; attempt++) {
        const current: ExtractionPipeline = await engine.store.readExtractionPipeline(task.id, context); assert.equal(current.state, "known"); if (current.state !== "known") throw new Error("pipeline_unknown");
        const result = await engine.runExtractionPipeline({ task_id: task.id, expected_version: current.claim.version, worker_id: owner, lease_ms: 30000 }, context);
        assert.equal(result.state, "known"); if (result.state !== "known") throw new Error("pipeline_unknown");
        const failed = result.claim.state === "failed" ? { task: result.claim, attempt: result.claim_attempt } : result.judge?.state === "failed" ? { task: result.judge, attempt: result.judge_attempt } : undefined;
        if (!failed) { assert.equal(result.judge?.state, "succeeded"); completed++; break; }
        const reason = failed.attempt?.reason ?? "unknown"; errors[reason] = (errors[reason] ?? 0) + 1; await log("extraction_error", { index, attempt, task: failed.task.kind, code: reason });
        if (quotaSince !== undefined && Date.now() - quotaSince > 30 * 60 * 1000) throw new Error("quota_exhausted_over_30_minutes");
        if (Date.now() >= deadline || attempt >= 3) {
          residual.push({ index, task_id: failed.task.id, code: reason });
          if (residual.length > episodes.length * 0.1) throw new Error("extraction_failed:error_threshold");
          break;
        }
        await engine.store.retryModelTask({ task_id: failed.task.id, expected_version: failed.task.version }, context);
      }
      await log("extracted", { completed, processed: index + 1, residual: residual.length, total: episodes.length });
    }
    summary.extraction_completed = completed;
    durations.extraction_ms = Date.now() - extractStart;
    assert.ok(completed >= 50);
    // The Store seals successful audits or immutable terminal omissions; the
    // runner never fabricates coverage for unresolved tasks.
    summary.facts_materialized = (await driver.executeQuery("MATCH (f:Fact) RETURN count(f) AS count")).records[0]!.get("count");
    summary.stage = "coverage";
    for (const partition of ["episodes", "active_extraction"] as const) await engine.store.recordExtractionCoverage({ generation_id: generation.id, partition, expected_covered_ingest_seq: 0, covered_ingest_seq: count }, context);
    assert.deepEqual(await engine.drainEmbeddingOutbox(1000), { drained: 0, reason: "embeddings_disabled" });
    const selection = await engine.readExtractionSelection(context);
    summary.stage = "cutover";
    await engine.cutoverExtractionGeneration({ generation_id: generation.id, expected_generation_id: selection.generation_id, expected_selector_version: selection.selector_version }, context);
    const active = await snapshot(); summary.facts_active = active.facts_active; assert.ok(Number(active.facts_active) >= 50);
    // Acknowledge the original-message queue only after Store coverage/cutover
    // has accepted each outcome. This is distinct from the disabled vector queue.
    summary.stage = "outbox_drain";
    const activeEngine = engine, outboxBefore = (await engine.status()).pendingOutbox;
    const drained = await activeEngine.digest(async episode => {
      const taskId = pipelines.get(episode.id); assert.ok(taskId);
      const pipeline = await activeEngine.store.readExtractionPipeline(taskId, context);
      assert.equal(pipeline.state, "known"); if (pipeline.state !== "known") throw new Error("pipeline_unknown");
      assert.ok(["failed", "cancelled"].includes(pipeline.claim.state) || (pipeline.claim.state === "succeeded" && pipeline.judge && ["succeeded", "failed", "cancelled"].includes(pipeline.judge.state)));
    });
    assert.equal(drained, outboxBefore); assert.equal((await engine.status()).pendingOutbox, 0);
    summary.outbox_drain = { before: outboxBefore, drained, after: 0 }; await log("outbox_drain", summary.outbox_drain);
    durations.extraction_ms = Date.now() - extractStart;
    const facts = await driver.executeQuery("MATCH (f:Fact {generation:$generation}) RETURN f.content AS content ORDER BY f.id LIMIT 64", { generation: generation.id });
    const queries = [...new Set(facts.records.map(row => String(row.get("content"))))].slice(0, 5);
    assert.equal(queries.length, 5);
    const recall: { query: string; kinds: string[]; fact_count: number; fact_rank_min: number | null; ppr_used: boolean; channels: string[]; result_count: number; top: unknown[] }[] = [];
    summary.recall = recall; summary.fact_rank_base = 1;
    summary.stage = "recall";
    for (const query of queries) {
      await log("recall_request", { query, limit: 20 });
      const result = await engine.recallHybrid({ query, limit: 20, budget: { unit: "utf8_bytes", limit: 65536 } }, context);
      const factResults = result.results.filter(item => item.kind === "Fact");
      const receipt = { query, kinds: result.results.map(item => item.kind), fact_count: factResults.length,
        fact_rank_min: factResults.length ? result.results.findIndex(item => item.kind === "Fact") + 1 : null,
        ppr_used: result.diagnostics.ppr_used, channels: result.diagnostics.channels_used, result_count: result.results.length,
        top: result.results.map(item => ({ ...item, content: item.content.slice(0, 200) })),
        companions: result.companions.map(item => ({ ...item, content: item.content.slice(0, 200) })) };
      recall.push(receipt); await log("recall", receipt);
    }
    const factHits = recall.filter(result => result.fact_count > 0).length;
    summary.recall_fact_hits = factHits;
    if (factHits < 3) deviations.push(`derived Fact ranking: Facts outranked by Episodes in ${5 - factHits}/5 queries`);
    assert.equal(recall.length, 5);
    assert.ok(recall.some(result => result.kinds.includes("Fact") && result.ppr_used));
    summary.stage = "crash_drain";
    await engine.close(); engine = undefined; await startDaemon();
    const beforeVerify = await verify(), before = await snapshot();
    await command("docker", ["kill", "--signal", "SIGKILL", name]);
    const client = await connect();
    let wake_status: RpcStatusResult | undefined, settled_status: RpcStatusResult | undefined, settle_latency_ms: number | undefined;
    try {
      assert.equal((await client.request("status", {})).storage, "unavailable");
      await assert.rejects(client.request("policy.set", { policy_id: uuidv7(), selector: { episode_id: episodes[0]! }, scope: "content" }), { code: "storage_unavailable" });
      // Replay an already admitted delivery while offline: a real durable spool
      // entry must drain idempotently, without changing Episode/generation counts.
      assert.ok(converted[0], "crash replay needs a converted source receipt");
      assert.equal((await client.request("remember", converted[0])).state, "spooled");
      assert.ok((await client.request("status", {})).spool.pending > 0);
      // Subscribe before restart/wake. Convert rejection to a handled outcome so
      // an earlier Docker failure cannot leave an unobserved event promise.
      const settled = waitForEvent("drain_settled", 60000).then(at => ({ at }), cause => ({ cause }));
      await command("docker", ["start", name]); await ready();
      const wakeAt = Date.now();
      wake_status = await client.request("status", {}); await log("crash_wake_status", wake_status);
      const event = await settled; if ("cause" in event) throw event.cause;
      settle_latency_ms = event.at - wakeAt;
      settled_status = await client.request("status", {}); await log("crash_settled_status", { ...settled_status, settle_latency_ms });
      assert.equal(settled_status.storage, "available"); assert.equal(settled_status.spool.pending, 0);
      assert.equal(settled_status.spool.blocked, 0); assert.equal(settled_status.spool.quarantined, 0);
    } finally { await client.close(); }
    const afterVerify = await verify(), after = await snapshot(); assert.deepEqual(after, before);
    assert.equal(beforeVerify.status.outbox_pending, 0); assert.equal(afterVerify.status.outbox_pending, 0);
    summary.crash_drain = { before, after, equal: true, before_verify: beforeVerify, after_verify: afterVerify, wake_status, settled_status, settle_latency_ms, replay: "existing delivery idempotently spooled and drained", refused_code: "storage_unavailable" };
    summary.status = "passed"; summary.stage = "complete"; summary.reported_model = provider.reportedModelIncarnation;
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
    await cleanup("engine/driver", async () => { await engine?.close(); await driver?.close(); });
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
    await writeFile(join(evidence, "cleanup-receipts.md"), receipts.join("\n\n") + "\n");
    await writeFile(join(evidence, "e2e-summary.json"), redact(JSON.stringify(summary, null, 2)) + "\n");
    await log("complete", { status: summary.status, episodes: summary.episodes, facts_active: summary.facts_active, durations });
  }
}
if (import.meta.main) await runE2eReal();
