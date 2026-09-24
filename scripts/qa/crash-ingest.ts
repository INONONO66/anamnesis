#!/usr/bin/env node
import assert from "node:assert/strict";
import { execFile, spawn } from "node:child_process";
import { mkdtemp, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createInterface, type Interface } from "node:readline";
import type { ChildProcess } from "node:child_process";
import { promisify } from "node:util";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "../../packages/core/node_modules/uuid/dist/esm/index.js";
import { RpcClient } from "../../app/anamnesis/client.ts";

const execute = promisify(execFile);
const image = "neo4j@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d";
const owner = `g5-crash-${process.pid}-${Date.now()}`;
const evidence = resolve(".omo/evidence/runtime-complete/g5");
// Bind-mounted roots must live under $HOME (colima/virtiofs shares only the home directory).
const qaParent = join(process.env["HOME"] ?? "/tmp", ".cache", "anamnesis-qa"); await mkdir(qaParent, { recursive: true, mode: 0o700 });
const parent = await mkdtemp(join(qaParent, "ana-g5-crash-"));
const root = join(parent, "runtime"), key = join(parent, "provider.json");
const password = `g5-${uuidv7()}`;
const name = `anamnesis-${owner}`;
const env: NodeJS.ProcessEnv = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_NEO4J_PASSWORD: password,
  ANAMNESIS_NEO4J_USER: "neo4j", ANAMNESIS_NEO4J_DATABASE: "neo4j", ANAMNESIS_NEO4J_CONTAINER: "", ANAMNESIS_QA_OWNER: owner,
  ANAMNESIS_LLM_BASE_URL: "http://127.0.0.1:1", ANAMNESIS_LLM_MODEL: "g5-inert", ANAMNESIS_LLM_API_KEY_FILE: key };
interface CrashSummary { status: string; accepted_before: number; spooled_during_outage: number; final_episodes: number; distinct_ids: number; spool_pending: number; error?: string }
const summary: CrashSummary = { status: "running", accepted_before: 0, spooled_during_outage: 0, final_episodes: 0, distinct_ids: 0, spool_pending: -1 };
let container = "", daemon: ChildProcess | undefined, lines: Interface | undefined, client: RpcClient | undefined;
const daemonStderr: string[] = []; const allStdout: string[] = [];
/** Event subscription, not polling: resolve on the Neo4j "Started." log line emitted after the given moment, bounded by a deadline. */
async function awaitNeo4jStarted(containerId: string, since: string, timeoutMs: number, code: string): Promise<void> {
  const logs = spawn("docker", ["logs", "-f", "--since", since, containerId], { stdio: ["ignore", "pipe", "pipe"] });
  const streams = [createInterface({ input: logs.stdout! }), createInterface({ input: logs.stderr! })];
  try {
    await new Promise<void>((resolveStarted, reject) => {
      const timer = setTimeout(() => reject(new Error(code)), timeoutMs);
      const onLine = (line: string) => { if (line.includes("Started.")) { clearTimeout(timer); resolveStarted(); } };
      for (const stream of streams) stream.on("line", onLine);
      logs.once("exit", exitCode => { clearTimeout(timer); reject(new Error(`${code}:docker_logs_exit_${exitCode}`)); });
    });
  } finally { for (const stream of streams) stream.close(); logs.kill(); }
}
async function freePort(): Promise<number> { const { createServer } = await import("node:net"); return new Promise((resolve, reject) => { const server = createServer(); server.once("error", reject); server.listen(0, "127.0.0.1", () => { const address = server.address(); server.close(() => typeof address === "object" && address ? resolve(address.port) : reject(new Error("no_port"))); }); }); }
async function docker(...args: string[]): Promise<string> { return (await execute("docker", args, { env, maxBuffer: 4 * 1024 * 1024 })).stdout.trim(); }
function episode(n: number) { return { episode: { schema: "anamnesis.original-message/1" as const, time: { value: "2026-09-19T00:00:00Z", precision: "second" as const }, content: `G5 outage episode ${n}`, origin: { source: "g5-crash", session: "owned", actor: "qa", record: String(n) }, mass: 1, properties: {} }, source_revision: `g5-${n}`, expected_previous_revision_key: null }; }
try {
  await mkdir(evidence, { recursive: true }); await mkdir(root, { recursive: true, mode: 0o700 }); await writeFile(key, JSON.stringify({ bearer: "inert-g5-fixture-bearer" }), { mode: 0o600 });
  // Reserve an explicit host port: an ephemeral "::7687" mapping is re-assigned by docker start, so the daemon's Bolt URI would never recover.
  const port = await freePort();
  const startedAt = new Date().toISOString();
  container = await docker("run", "-d", "--name", name, "--label", `anamnesis.qa.owner=${owner}`, "-p", `127.0.0.1:${port}:7687`, "-e", `NEO4J_AUTH=neo4j/${password}`, image);
  const uri = `bolt://127.0.0.1:${port}`;
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
  await awaitNeo4jStarted(container, startedAt, 90000, "bolt_readiness_timeout"); await driver.verifyConnectivity(); await driver.close();
  env.ANAMNESIS_NEO4J_URI = uri;
  daemon = spawn(process.execPath, [resolve("dist/anamnesis-ops.mjs"), "foreground"], { env, stdio: ["ignore", "pipe", "pipe"] });
  lines = createInterface({ input: daemon.stdout! });
  lines.on("line", (line) => { allStdout.push(`${new Date().toISOString()} ${line}`); });
  daemon.stderr!.on("data", (chunk: Buffer) => { daemonStderr.push(chunk.toString()); });
  await new Promise<void>((resolveReady, reject) => { const timer = setTimeout(() => reject(new Error("daemon_readiness_timeout")), 90000); const onLine = (line: string) => { if (line.includes('"event":"listening"')) { clearTimeout(timer); lines?.off("line", onLine); resolveReady(); } }; lines?.on("line", onLine); daemon?.once("error", reject); daemon?.once("exit", (code) => { clearTimeout(timer); reject(new Error(`daemon_exited_${code}:${daemonStderr.join("").trim().slice(0, 200)}`)); }); });
  client = await RpcClient.connect(join(root, "anamnesis.sock"), (await readFile(join(root, "token"), "utf8")).trim());
  assert.ok(client);
  const before = Array.from({ length: 100 }, (_, i) => episode(i));
  for (const params of before) { const result: any = await client.request("remember", params); assert.equal(result.state, "committed"); summary.accepted_before++; }
  await docker("kill", "--signal", "SIGKILL", container);
  const during = Array.from({ length: 50 }, (_, i) => episode(100 + i));
  for (const params of during) { const result: any = await client.request("remember", params); assert.equal(result.state, "spooled"); summary.spooled_during_outage++; }
  const settled = new Promise<void>((resolveSettled, reject) => { const timer = setTimeout(() => reject(new Error("drain_settled_timeout")), 90000); const onLine = (line: string) => { if (line === JSON.stringify({ event: "drain_settled" })) { clearTimeout(timer); lines?.off("line", onLine); resolveSettled(); } }; lines?.on("line", onLine); });
  const restartedAt = new Date().toISOString();
  await docker("start", container);
  assert.equal(Number((await docker("port", container, "7687/tcp")).split(":").at(-1)), port, "bolt host port must survive restart");
  // refresh() is demand-driven: wait for Bolt to accept connections (event: successful verifyConnectivity),
  // then a single status request lets the daemon observe recovery and wake its drain loop. No timers.
  const daemonEvents: string[] = []; const onEvent = (line: string) => { daemonEvents.push(line); }; lines.on("line", onEvent);
  await awaitNeo4jStarted(container, restartedAt, 90000, "bolt_restart_timeout");
  const wake: any = await client.request("status", {}); daemonEvents.push(JSON.stringify({ event: "qa_wake_status", storage: wake.storage, pending: wake.spool.pending }));
  try { await settled; } finally { lines.off("line", onEvent); await writeFile(join(evidence, "crash-ingest-daemon-events.log"), daemonEvents.join("\n") + "\n"); }
  const after = await client.request("status", {}); summary.spool_pending = after.spool.pending; assert.equal(after.storage, "available"); assert.equal(after.spool.pending, 0);
  const verifyDriver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
  try {
    await verifyDriver.verifyConnectivity();
    const result = await verifyDriver.executeQuery("MATCH (e:Element:Episode) RETURN count(e) AS episodes, count(DISTINCT e.id) AS distinct_ids");
    const row = result.records[0]; assert.ok(row);
    summary.final_episodes = Number(row.get("episodes")); summary.distinct_ids = Number(row.get("distinct_ids"));
  } finally { await verifyDriver.close(); }
  assert.equal(summary.final_episodes, 150); assert.equal(summary.distinct_ids, 150); summary.status = "passed";
} catch (error) {
  summary.status = "failed"; summary.error = error instanceof Error && /^Command failed: docker/.test(error.message) ? "docker_unavailable" : error instanceof Error ? error.message.replace(/[^a-zA-Z0-9_.:-]/g, "_") : "unknown"; process.exitCode = 1;
} finally {
  await writeFile(join(evidence, "crash-ingest-summary.json"), JSON.stringify(summary, null, 2) + "\n");
  if (daemon) await writeFile(join(evidence, "crash-ingest-daemon-stderr.log"), daemonStderr.join("") + `\nexit_code=${daemon.exitCode}\n---stdout---\n` + allStdout.join("\n") + "\n");
  if (client) await client.close().catch(() => {}); if (daemon && daemon.exitCode === null) daemon.kill("SIGTERM");
  if (container) await execute("docker", ["rm", "-f", "-v", container]).catch(() => {});
  await rm(parent, { recursive: true, force: true });
}
