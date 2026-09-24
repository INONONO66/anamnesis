// Real Node daemon over UDS: the embedding outbox worker must drain committed
// Episodes by itself, inside the single-writer turn loop. Every wait is an
// event (stdout line, socket reply, child exit) with a bounded deadline.
// The drain scenario needs the isolated harness graph and is skipped without it:
//   bun scripts/qa/runtime-scenarios.ts --case foundation --evidence-root .omo/evidence/foundation/daemon-workers --test-path app/anamnesis/daemon-workers.test.ts
import { afterAll, beforeAll, expect, test } from "bun:test";
import { execFile, spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { mkdtemp, rm } from "node:fs/promises";
import { createServer, type Server } from "node:http";
import type { AddressInfo } from "node:net";
import { join } from "node:path";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import neo4j from "neo4j-driver";
import { RpcClient } from "./client.ts";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const TOKEN = "installation-token-fixture";
const URI = process.env["ANAMNESIS_TEST_NEO4J_URI"], PASSWORD = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
const deadline = (ms = 20_000) => AbortSignal.timeout(ms);
const profile = { model: "outbox-worker-fixture", model_incarnation: "e".repeat(64), dimensions: 3,
  document_prefix: "document: ", query_prefix: "query: ", max_input_bytes: 65536, norm: "unit_l2", norm_tolerance: 0.001 };

let build: string, bundle: string;
beforeAll(async () => {
  build = await mkdtemp("/tmp/ana-workers-bundle-");
  bundle = join(build, "main.mjs");
  await promisify(execFile)(process.execPath, ["build", "app/anamnesis/main.ts", "--target=node", "--outfile", bundle], { cwd: ROOT });
});
afterAll(async () => { await rm(build, { recursive: true, force: true }); });

/** Deterministic HttpEmbeddingProvider peer: echoes the profile identity and one unit vector per call. */
async function embedder(): Promise<{ server: Server; endpoint: string; calls: () => number }> {
  let calls = 0;
  const server = createServer(async (request, response) => {
    const chunks: Buffer[] = []; for await (const chunk of request) chunks.push(chunk as Buffer);
    JSON.parse(Buffer.concat(chunks).toString("utf8")); calls++;
    response.setHeader("content-type", "application/json");
    response.end(JSON.stringify({ model: profile.model, model_incarnation: profile.model_incarnation, data: [{ index: 0, embedding: [0, 1, 0] }] }));
  });
  const listening = once(server, "listening", { signal: deadline(5000) });
  server.listen(0, "127.0.0.1");
  await listening;
  return { server, endpoint: `http://127.0.0.1:${(server.address() as AddressInfo).port}/v1/embeddings`, calls: () => calls };
}

/** Buffered stdout subscription: lines are kept from the moment of attachment, so an await never misses an earlier event. */
function stdoutLines(child: ChildProcess) {
  const buffered: Array<Record<string, unknown>> = [];
  const waiters: Array<(line: Record<string, unknown>) => void> = [];
  createInterface({ input: child.stdout! }).on("line", text => {
    const line = JSON.parse(text) as Record<string, unknown>;
    const waiter = waiters.shift();
    if (waiter) waiter(line); else buffered.push(line);
  });
  const next = () => new Promise<Record<string, unknown>>((resolve, reject) => {
    if (buffered.length) return resolve(buffered.shift()!);
    const signal = deadline();
    signal.addEventListener("abort", () => reject(signal.reason), { once: true });
    waiters.push(resolve);
  });
  return { next, until: async (event: string) => { for (;;) { const line = await next(); if (line["event"] === event) return line; } } };
}

interface Daemon { root: string; child: ChildProcess; lines: ReturnType<typeof stdoutLines>; exit: Promise<[number | null, NodeJS.Signals | null]>; stderr(): string; }
async function fixture(extra: NodeJS.ProcessEnv, run: (daemon: Daemon) => Promise<void>): Promise<void> {
  const root = await mkdtemp("/tmp/ana-workers-");
  let child: ChildProcess | undefined;
  try {
    const env: NodeJS.ProcessEnv = { ...process.env };
    for (const name of Object.keys(env)) if (/^ANAMNESIS_(LLM_|EMBEDDING_|EXTRACTION_|LISTEN)/.test(name)) delete env[name];
    Object.assign(env, { ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: TOKEN, ANAMNESIS_NEO4J_PASSWORD: "unused-offline", ANAMNESIS_NEO4J_URI: "bolt://127.0.0.1:1" }, extra);
    child = spawn("node", [bundle], { env, stdio: ["ignore", "pipe", "pipe"] });
    let stderr = "";
    child.stderr!.on("data", (bytes: Buffer) => { stderr += bytes; });
    const exit = once(child, "exit") as Promise<[number | null, NodeJS.Signals | null]>;
    const lines = stdoutLines(child);
    await Promise.race([lines.until("listening"), exit.then(([code]) => { throw new Error(`daemon exited ${code} before listening: ${stderr}`); })]);
    await run({ root, child, lines, exit, stderr: () => stderr });
  } finally {
    if (child && child.exitCode === null && child.signalCode === null) {
      const exited = once(child, "exit", { signal: deadline() });
      child.kill("SIGKILL");
      await exited;
    }
    await rm(root, { recursive: true, force: true });
  }
}

const episode = (root: string, record: string) => ({ episode: { schema: "anamnesis.original-message/1" as const, content: `worker fixture ${record}`,
  time: { value: "2026-09-01T00:00:00Z", precision: "second" as const }, mass: 0, properties: {},
  origin: { source: root, session: "workers", actor: "fixture", record } }, source_revision: "v1", expected_previous_revision_key: null });

test("status reports the worker counters even while storage is unavailable", async () => {
  await fixture({}, async daemon => {
    const client = await RpcClient.connect(join(daemon.root, "anamnesis.sock"), TOKEN);
    try {
      const status = await client.request("status", {});
      expect(status.storage).toBe("unavailable");
      expect(status.workers).toEqual({ embedding: { pending: null, drained_total: 0, quarantined_total: 0, last_error: null }, extraction: { state: "unconfigured" } });
      const exited = once(daemon.child, "exit", { signal: deadline() });
      expect((await client.request("shutdown", {})).state).toBe("stopping");
      expect((await exited)[0]).toBe(0);
    } finally { await client.close(); }
  });
}, 60_000);

test.skipIf(!URI || !PASSWORD)("committed remembers wake the embedding worker, which drains the outbox and reports idle", async () => {
  const provider = await embedder();
  const driver = neo4j.driver(URI!, neo4j.auth.basic("neo4j", PASSWORD!), { disableLosslessIntegers: true });
  try {
    await fixture({ ANAMNESIS_NEO4J_URI: URI!, ANAMNESIS_NEO4J_PASSWORD: PASSWORD!,
      ANAMNESIS_EMBEDDING_CONFIG: JSON.stringify({ endpoint: provider.endpoint, profile, timeout_ms: 5000 }) }, async daemon => {
      const client = await RpcClient.connect(join(daemon.root, "anamnesis.sock"), TOKEN);
      try {
        expect((await client.request("status", {})).capabilities.embeddings).toBe(true);
        const ids: string[] = [];
        for (const record of ["one", "two", "three"]) {
          const result = await client.request("remember", episode(daemon.root, record));
          expect(result.state).toBe("committed");
          ids.push((result as { id: string }).id);
        }
        // The worker may report idle between commits as well; each idle line is a
        // real transition, and the one after the third commit shows everything drained.
        let status = await client.request("status", {});
        while (status.workers.embedding.drained_total < 3) {
          await daemon.lines.until("workers_idle");
          status = await client.request("status", {});
        }
        expect(status.workers).toEqual({ embedding: { pending: 0, drained_total: 3, quarantined_total: 0, last_error: null }, extraction: { state: "unconfigured" } });
        expect(status.outbox_pending).toBe(0);
        expect(provider.calls()).toBe(3);
        const vectors = await driver.executeQuery("MATCH (v:EmbeddingVector) WHERE v.episode_id IN $ids RETURN count(v) AS n", { ids });
        expect(vectors.records[0]!.get("n")).toBe(3);
        const exited = once(daemon.child, "exit", { signal: deadline() });
        expect((await client.request("shutdown", {})).state).toBe("stopping");
        expect((await exited)[0]).toBe(0);
      } finally { await client.close(); }
    });
  } finally {
    await driver.close();
    const closed = once(provider.server, "close", { signal: deadline(5000) });
    provider.server.close(); provider.server.closeAllConnections();
    await closed;
  }
}, 120_000);
