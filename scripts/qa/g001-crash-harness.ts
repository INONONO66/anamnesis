import assert from "node:assert/strict";
import { test } from "node:test";
import { spawn, type ChildProcess } from "node:child_process";
import { EventEmitter, once } from "node:events";
import { createServer, connect, type Socket } from "node:net";
import { createInterface } from "node:readline";
import { appendFileSync, writeFileSync } from "node:fs";
import { mkdir, mkdtemp, readFile, readdir, rm, stat, cp } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createHash, randomBytes, randomUUID } from "node:crypto";
import neo4j, { EagerResult } from "neo4j-driver";
import { Store } from "../../packages/core/src/store.ts";
import { EpisodeJournal } from "../../packages/core/src/journal.ts";
import { DurableSpool } from "../../packages/core/src/spool.ts";
import { RpcClient } from "../../app/anamnesis/client.ts";
import type { RpcRememberParams, RpcRememberResult } from "../../packages/protocol/src/rpc.ts";

const root = process.cwd();
const evidence = resolve(root, process.env["G001_CRASH_EVIDENCE_ROOT"] ?? ".omo/evidence/g001-crash-recovery");
const bun = process.env["BUN"] ?? "/tmp/anamnesis-bun-141-lzhkuW/node_modules/@oven/bun-darwin-aarch64/bin/bun";
const sha = (value: string | Buffer) => createHash("sha256").update(value).digest("hex");
// Independent canonical oracle: JSON property-list ordering, not runtime helper.
function canonical(value: unknown): string {
  const keys = new Set<string>();
  const visit = (v: unknown) => { if (v && typeof v === "object") for (const [k, child] of Object.entries(v)) { if (!Array.isArray(v)) keys.add(k); visit(child); } };
  visit(value); return JSON.stringify(value, [...keys].sort());
}
const identity = (r: RpcRememberResult) => ({ revision_key: r.revision_key, body_digest: r.body_digest, data_incarnation: r.data_incarnation });
const input = (record: string, minute: string): RpcRememberParams => ({ episode: { schema: "anamnesis.original-message/1", time: { value: `2026-09-09T00:${minute}:00Z`, precision: "second" }, content: `G001 ${record}`, origin: { source: "g001-crash", session: "ordered-crash-session", actor: "fixture", record }, mass: 1, properties: { fixture: true } }, source_revision: "v1", expected_previous_revision_key: null });
// Exact reconstructed post167/pre194 contracts from legacy-integrity.test.ts;
// these bytes are historical inspection fixtures, not a legacy import API.
const legacyLines = [
  '{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.original-message/1","content":"Ino prefers dark mode.","origin":{"source":"slack","session":"C0123/2026-08-21","actor":"U098765","record":"1724221402.000300"},"mass":0.5,"properties":{}}}\n',
  '{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.claim/1","content":"Ino prefers dark mode.","origin":{"source":"slack","session":"C0123/2026-08-21","actor":"U098765","record":"1724221402.000300"},"mass":0.5,"properties":{}}}\n',
  '{"recordedAt":"2026-09-02T12:34:56.000Z","element":{"schema":"anamnesis.claim/1","time":{"value":"2026-08-21T14:03:22+09:00","precision":"second"},"content":"Ino prefers dark mode.","origin":{"source":"slack","session":"C0123/2026-08-21","actor":"U098765","record":"1724221402.000300"},"mass":0.5,"properties":{"sub_kind":"opinion"}}}\n',
];
const legacyHashes = ["49db3f55710c6c97fd63f08fd02cc4cbd05b6087bb76b0f88172603471c2de56", "983f4439c13cf3bf8aeb877b73861cf4a3e0335970cffcb512df9810b893542c", "2149154d303dd0a84da9714b5acb3ba3bc1f580199c7c8679e3f6c3256ec10de"];
const legacyDigests = ["25336ecb71d5a5703763925ee83621b5f014a2c2ea4f6051993018c390b181e1", "4d0514a31ade66835f45f840dd0609cf967a1b2a57cb37e30b4808f60c99c31b", "56ab9d7ca2f8f00995e99a6efe75fa8b8be2609b137141a630dba94db21085af"];
const legacyIds = ["0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f", "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e10", "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e11"];
const unknownOutcome = (error: unknown) => ({ success: false as const, error: String(error), code: error instanceof Error && "code" in error ? error.code : null });
interface ProcessResult { code: number | null; signal: NodeJS.Signals | null; output: string; }
interface Launched { child: ChildProcess; done: Promise<ProcessResult>; startup: Promise<unknown[][]>; wait(event: string, ms?: number): Promise<unknown[]>; stop(): Promise<ProcessResult>; }
const deadline = (ms = 30_000) => AbortSignal.timeout(ms);
async function absent(path: string) { await assert.rejects(stat(path), { code: "ENOENT" }); }
async function hashes(paths: string[]) {
  return Object.fromEntries(await Promise.all(paths.sort().map(async p => [p, sha(await readFile(resolve(root, p)))])));
}
async function sourcePaths(path: string): Promise<string[]> {
  const items = await readdir(path, { withFileTypes: true });
  return (await Promise.all(items.map(async e => e.isDirectory() ? sourcePaths(join(path, e.name)) : /\.(ts|json)$/.test(e.name) ? [join(path, e.name)] : []))).flat();
}

test("real crash recovery: exact identity/topology, legacy preservation and UDS UNKNOWN", { timeout: 360_000 }, async () => {
  assert.equal(process.env["G001_OWNED_DB_GRANTED"], "1", "explicit DB slot grant required; no implicit database execution");
  await mkdir(evidence, { recursive: true });
  // Refuse reuse even after a failed run; historical receipts are immutable.
  writeFileSync(join(evidence, "run-owner.json"), JSON.stringify({ task: "st_01a0852d", pid: process.pid }), { flag: "wx" });
  const owner = randomUUID(), name = `anamnesis-g001-${owner}`;
  const password = randomBytes(24).toString("base64url"), token = randomBytes(24).toString("base64url");
  const redact = (s: string) => s.replaceAll(password, "[REDACTED]").replaceAll(token, "[REDACTED]");
  const record = (file: string, value: unknown) => writeFileSync(join(evidence, file), redact(JSON.stringify(value, null, 2) + "\n"));
  const log = (file: string, value: unknown) => appendFileSync(join(evidence, file), redact(JSON.stringify(value) + "\n"));
  const children: Launched[] = [], clients = new Set<RpcClient>(), sockets = new Set<Socket>();
  const roots: string[] = [];
  function launch(command: string, args: string[], env = process.env, ipc = false, startupEvents: string[] = []): Launched {
    log("commands.jsonl", { command, args, cwd: root });
    const events = new EventEmitter(); let output = ""; let terminal = false;
    // Register startup observations before spawning even the ordinary entry.
    // Listening is availability, not evidence that its replay has settled.
    const startupController = new AbortController();
    const observedStartup = Promise.all(startupEvents.map(event => once(events, event, { signal: AbortSignal.any([startupController.signal, deadline()]) })));
    const child = spawn(command, args, { cwd: root, env, stdio: ipc ? ["ignore", "pipe", "pipe", "ipc"] : ["ignore", "pipe", "pipe"] });
    const readers = [child.stdout!, child.stderr!].map(stream => {
      stream.on("data", b => { output += b; log("process-output.jsonl", { pid: child.pid, text: String(b) }); });
      const lines = createInterface({ input: stream });
      lines.on("line", line => {
        if (/ INFO\s+Started\.$/.test(line)) events.emit("db-ready");
        if (line.startsWith("{")) { const value = JSON.parse(line); if (value.event) events.emit(value.event, value); }
      }); return lines;
    });
    child.on("message", message => {
      if (message && typeof message === "object" && "event" in message) { log("ipc.jsonl", { pid: child.pid, message }); events.emit(String(message.event), message); }
    });
    const done = new Promise<{ code: number | null; signal: NodeJS.Signals | null; output: string }>((resolveDone, reject) => {
      child.once("error", reject);
      child.once("close", (code, signal) => { terminal = true; for (const r of readers) r.close(); const result = { code, signal, output }; log("exits.jsonl", { pid: child.pid, command, args, ...result }); resolveDone(result); });
    });
    const wait = (event: string, ms = 30_000) => {
      const controller = new AbortController();
      const signal = AbortSignal.any([controller.signal, deadline(ms)]);
      const observed = once(events, event, { signal });
      return Promise.race([observed, done.then(result => { throw new Error(`process exited awaiting ${event}: ${JSON.stringify(result)}`); })]).finally(() => controller.abort());
    };
    const startup = Promise.race([observedStartup, done.then(result => { throw new Error(`process exited awaiting startup: ${JSON.stringify(result)}`); })]).finally(() => startupController.abort());
    const result = { child, done, startup, wait, async stop() {
      if (!terminal && child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
      return Promise.race([done, once(deadline(10_000), "abort").then(() => { throw new Error("child cleanup deadline"); })]);
    } };
    children.push(result); log("resources.jsonl", { pid: child.pid, command, args, owner }); return result;
  }
  async function command(exe: string, args: string[], env = process.env) {
    const p = launch(exe, args, env); const signal = deadline(120_000);
    const abort = () => p.child.kill("SIGKILL"); signal.addEventListener("abort", abort, { once: true });
    try { const result = await p.done; assert.equal(result.code, 0, result.output); return result.output.trim(); }
    finally { signal.removeEventListener("abort", abort); }
  }
  const docker = (args: string[]) => command("docker", args, { ...process.env, NEO4J_AUTH: `neo4j/${password}` });
  const track = (s: Socket) => { sockets.add(s); s.once("close", () => sockets.delete(s)); return s; };
  let target = 0;
  const relay = createServer(socket => {
    track(socket);
    if (!target) { socket.destroy(); return; }
    const upstream = track(connect(target, "127.0.0.1"));
    socket.on("error", e => { log("transport.jsonl", { error: String(e) }); upstream.destroy(); });
    upstream.on("error", e => { log("transport.jsonl", { error: String(e) }); socket.destroy(); });
    socket.on("close", () => upstream.destroy()); upstream.on("close", () => socket.destroy());
    socket.pipe(upstream); upstream.pipe(socket);
  });
  let proxy: ReturnType<typeof createServer> | undefined, attempted = false;
  let db: ReturnType<typeof neo4j.driver> | undefined;
  let legacyStore: Store | undefined;
  let failure: unknown;
  const cleanup: unknown[] = [];
  try {
    const files = [...await sourcePaths("app/anamnesis"), ...await sourcePaths("packages/core/src"), ...await sourcePaths("packages/protocol/src"), "scripts/qa/g001-crash-harness.ts", "scripts/qa/g001-crash-child.ts", "package.json", "bun.lock"];
    const before = await hashes(files); record("source-before.json", before);
    record("git.json", { head: await command("git", ["rev-parse", "HEAD"]), tree: await command("git", ["rev-parse", "HEAD^{tree}"]), status: await command("git", ["status", "--short"]), node: process.version });
    const ordinary = join(evidence, "daemon.mjs"), fixture = join(evidence, "crash-child.mjs");
    for (const [source, out] of [["app/anamnesis/main.ts", ordinary], ["scripts/qa/g001-crash-child.ts", fixture]]) await command(bun, ["build", source!, "--target=node", `--outfile=${out}`]);
    assert.deepEqual(await hashes(files), before, "source changed during private build");
    record("artifact-hashes.json", await hashes([ordinary, fixture, process.argv[1]!]));
    const bound = once(relay, "listening", { signal: deadline() }); relay.listen(0, "127.0.0.1"); await bound;
    const address = relay.address(); assert.ok(address && typeof address === "object");
    const envFor = (r: string) => ({ ...process.env, ANAMNESIS_RUNTIME_ROOT: r, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: `bolt://127.0.0.1:${address.port}`, ANAMNESIS_NEO4J_PASSWORD: password, ANAMNESIS_NEO4J_USER: "neo4j", ANAMNESIS_NEO4J_DATABASE: "neo4j" });
    const runtimeRoot = await mkdtemp("/tmp/ana-g001-"); roots.push(runtimeRoot); log("resources.jsonl", { root: runtimeRoot, owner });
    async function start(r: string, instrumented = false, mutant = false) {
      const waitForDrain = target > 0 && (!instrumented || mutant);
      const p = launch(process.execPath, [instrumented ? fixture : ordinary, ...(mutant ? ["--drop-replay"] : [])], envFor(r), instrumented,
        ["listening", ...(waitForDrain ? ["drain_settled"] : [])]);
      const trigger = async () => { if (instrumented) { await p.wait("fixture-ready"); p.child.send!({ command: "start" }); } };
      await Promise.all([p.startup, trigger()]);
      log("startup-observed.jsonl", { pid: p.child.pid, root: r, instrumented, mutant, drainSettled: waitForDrain });
      return p;
    }
    async function client(r: string) { const c = await RpcClient.connect(join(r, "anamnesis.sock"), token); clients.add(c); return c; }
    async function stopGracefully(p: ReturnType<typeof launch>, c: RpcClient, r: string) {
      assert.equal((await c.request("shutdown", {})).state, "stopping"); await c.close(); clients.delete(c);
      const ended = await Promise.race([p.done, once(deadline(), "abort").then(() => { throw new Error("graceful shutdown deadline"); })]);
      assert.equal(ended.code, 0); await absent(join(r, "anamnesis.sock")); await absent(join(r, "owner"));
    }
    let daemon = await start(runtimeRoot, true), c = await client(runtimeRoot);
    const inputs = [input("one", "00"), input("two", "10")], receipts: RpcRememberResult[] = [];
    record("topology-inputs.json", inputs);
    for (const [i, params] of inputs.entries()) {
      const r = await c.request("remember", params); assert.equal(r.state, "spooled");
      assert.equal(r.body_digest, sha(canonical({ digest_version: 1, params })));
      const o = params.episode.origin; assert.equal(r.revision_key, sha(JSON.stringify([sha(JSON.stringify([o.source, o.session, o.actor, o.record])), params.source_revision])));
      if (r.state === "spooled") assert.equal(r.spool_seq, i + 1); receipts.push(r);
    }
    record("offline-receipts.json", receipts);
    const offline = await c.request("status", {}); assert.equal(offline.spool.pending, 2);
    // Real offline restart proves retained receipt identity independently of DB.
    await stopGracefully(daemon, c, runtimeRoot);
    daemon = await start(runtimeRoot, true); c = await client(runtimeRoot);
    const offlineRestart = await c.request("status", {});
    assert.equal(offlineRestart.data_incarnation, offline.data_incarnation); assert.notEqual(offlineRestart.fs_epoch, offline.fs_epoch);
    for (const [i, params] of inputs.entries()) assert.deepEqual(await c.request("remember", params), receipts[i]);
    record("offline-restart.json", offlineRestart);
    attempted = true;
    await docker(["create", "--name", name, "--label", `anamnesis.qa.owner=${owner}`, "-p", "127.0.0.1::7687", "-e", "NEO4J_AUTH", "-e", "NEO4J_server_memory_heap_max__size=512M", "-e", "NEO4J_server_memory_pagecache_size=256M", "neo4j:5.26-community"]);
    const attached = launch("docker", ["start", "-a", name]); await attached.wait("db-ready", 180_000);
    target = Number(await docker(["inspect", "-f", '{{(index (index .NetworkSettings.Ports "7687/tcp") 0).HostPort}}', name]));
    assert.ok(Number.isSafeInteger(target) && target > 0);
    db = neo4j.driver(`bolt://127.0.0.1:${target}`, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true, connectionTimeout: 10_000, connectionAcquisitionTimeout: 10_000, maxTransactionRetryTime: 0 });
    await db.verifyConnectivity();
    const rows = async () => (await db!.executeQuery("MATCH (e:Element:Episode) RETURN properties(e) AS e ORDER BY e.ingest_seq")).records.map(r => r.get("e"));
    assert.deepEqual(await rows(), []);
    const edgeQuery = "MATCH (a)-[r:NEXT_EPISODE]->(b) RETURN a.id AS from,b.id AS to,properties(r) AS props ORDER BY a.ingest_seq,b.ingest_seq,r.id";
    const edges = async () => (await db!.executeQuery(edgeQuery)).records.map(r => r.toObject());
    const metaSequence = async () => (await db!.executeQuery("MATCH (m:Meta {key:'meta'}) RETURN m.ingest_seq AS seq")).records[0]!.get("seq");
    const sessionKey = sha(JSON.stringify([inputs[0]!.episode.origin.source, inputs[0]!.episode.origin.session]));
    function edgeOracle(actual: Awaited<ReturnType<typeof edges>>, episodes: Record<string, unknown>[]) {
      const expected = episodes.slice(1).map((episode, i) => ({ from: episodes[i]!["id"], to: episode["id"], key: sha(JSON.stringify([sessionKey, episodes[i]!["id"], episode["id"]])) }));
      assert.deepEqual(actual.map(edge => ({ from: edge["from"], to: edge["to"], key: edge["props"].idem_key })), expected, "exact NEXT_EPISODE endpoints and independent session/from/to keys");
    }
    // Recovery status may return before drain. Only after complete(1) holds
    // the real serial owner do we submit the request whose outcome will be lost.
    const parked = daemon.wait("complete-parked");
    assert.equal((await c.request("status", {})).storage, "available");
    const [event] = await parked; assert.deepEqual(event, { event: "complete-parked", sequence: 1 });
    const received = daemon.wait("request-received");
    let settled = false;
    const request = c.request("remember", inputs[0]!).then(value => { settled = true; return { success: true as const, value }; }, error => { settled = true; return unknownOutcome(error); });
    const [admission] = await received;
    assert.ok(admission && typeof admission === "object" && "request" in admission);
    assert.deepEqual(admission.request, { jsonrpc: "2.0", id: 6, method: "remember", params: inputs[0] });
    assert.equal(settled, false);
    record("pending-at-completion.json", { boundary: "remember frame received after real completion(1) parked; serial owner cannot dispatch it before release or SIGKILL", admission, settled });
    const committedBeforeKill = await rows(); record("db-before-kill.json", committedBeforeKill);
    const verifyRow = (row: Record<string, unknown>, params: RpcRememberParams, receipt: RpcRememberResult, seq: number) => {
      const origin = params.episode.origin, originHash = sha(JSON.stringify([origin.source, origin.session, origin.actor, origin.record]));
      assert.equal(row["origin_key"], originHash); assert.equal(row["session_key"], sha(JSON.stringify([origin.source, origin.session])));
      assert.equal(receipt.revision_key, sha(JSON.stringify([originHash, params.source_revision])));
      assert.equal(receipt.body_digest, sha(canonical({ digest_version: 1, params })));
      assert.equal(row["revision_key"], receipt.revision_key); assert.equal(row["content"], params.episode.content);
      assert.equal(row["source_revision"], params.source_revision); assert.equal(row["mass"], params.episode.mass);
      assert.equal(row["time_value"], params.episode.time.value); assert.equal(row["time_precision"], params.episode.time.precision);
      assert.deepEqual(JSON.parse(String(row["properties"])), params.episode.properties);
      for (const key of ["source", "session", "actor", "record"] as const) assert.equal(row[`origin_${key}`], params.episode.origin[key]);
      assert.equal(row["digest_format"], "rfc8785-v1");
      assert.equal(row["digest"], sha(canonical({ schema: params.episode.schema, content: params.episode.content, properties: params.episode.properties, time: params.episode.time, payload_hash: null, previous_revision_key: null })));
      assert.equal(row["ingest_seq"], seq); assert.match(String(row["id"]), /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/);
    };
    assert.equal(committedBeforeKill.length, 1); verifyRow(committedBeforeKill[0], inputs[0]!, receipts[0]!, 1);
    const edgesBeforeKill = await edges(); edgeOracle(edgesBeforeKill, committedBeforeKill);
    record("topology-before-kill.json", { sessionKey, rows: committedBeforeKill, edges: edgesBeforeKill });
    const spool = new DurableSpool(join(runtimeRoot, "spool"));
    const retained = await spool.page({ limit: 100 }); assert.deepEqual(retained.entries.map(e => e.sequence), [1, 2]);
    record("retained-before-complete.json", retained);
    const protectedPaths = (await readdir(join(runtimeRoot, "spool"))).map(f => join(runtimeRoot, "spool", f));
    const protectedHashes = await hashes(protectedPaths); record("protected-before-kill.json", protectedHashes);
    assert.equal((await daemon.stop()).signal, "SIGKILL"); const outcome = await request;
    assert.equal(outcome.success, false); if (!outcome.success) assert.equal(outcome.code, "outcome_unknown"); record("unknown-client-outcome.json", outcome);
    assert.deepEqual(await hashes(protectedPaths), protectedHashes); await c.close(); clients.delete(c);
    // A separate copied root and fixture-only replay mutation run the SAME
    // recovery oracle. Sequence 2 was never committed, so hiding it must fail.
    const mutantRoot = await mkdtemp("/tmp/ana-g001-mut-"); roots.push(mutantRoot);
    for (const item of await readdir(runtimeRoot)) if (!["anamnesis.sock", "owner"].includes(item)) await cp(join(runtimeRoot, item), join(mutantRoot, item), { recursive: true });
    const mutant = await start(mutantRoot, true, true), mc = await client(mutantRoot);
    async function recoveryOracle(connection: RpcClient) {
      const result = await connection.request("ingest.status", identity(receipts[1]!));
      assert.equal(result.state, "committed"); return result;
    }
    let mutationFailure: unknown;
    try { await recoveryOracle(mc); } catch (error) { mutationFailure = error; }
    assert.ok(mutationFailure instanceof assert.AssertionError); record("mutation-red.json", { expected: "committed", error: String(mutationFailure), actual: mutationFailure.actual });
    await stopGracefully(mutant, mc, mutantRoot);
    daemon = await start(runtimeRoot); c = await client(runtimeRoot);
    await recoveryOracle(c);
    const recovered = await c.request("status", {}); assert.equal(recovered.spool.pending, 0);
    assert.equal(recovered.data_incarnation, offline.data_incarnation); assert.notEqual(recovered.fs_epoch, offlineRestart.fs_epoch);
    const after = await rows(); assert.equal(after.length, 2); assert.deepEqual(after[0], committedBeforeKill[0]);
    const recoveredEdges = await edges(); edgeOracle(recoveredEdges, after);
    // Real DB fixture mutation inside a rolled-back transaction; run the exact
    // same edge oracle on its query, not a substitute count assertion.
    const mutationSession = db.session(), mutationTx = mutationSession.beginTransaction();
    try {
      const deleted = await mutationTx.run("MATCH ()-[r:NEXT_EPISODE]->() DELETE r");
      assert.equal(deleted.summary.counters.updates().relationshipsDeleted, 1);
      const missingEdges = (await mutationTx.run(edgeQuery)).records.map(r => r.toObject());
      let edgeFailure: unknown;
      try { edgeOracle(missingEdges, after); } catch (error) { edgeFailure = error; }
      assert.ok(edgeFailure instanceof assert.AssertionError);
      record("topology-mutation-red.json", { mutation: "delete real NEXT_EPISODE in rollback-only transaction", actual: edgeFailure.actual, expected: edgeFailure.expected, error: String(edgeFailure) });
    } finally { await mutationTx.rollback(); await mutationSession.close(); }
    assert.deepEqual(await edges(), recoveredEdges);
    for (const [i, params] of inputs.entries()) {
      verifyRow(after[i], params, receipts[i]!, i + 1);
      const retry = await c.request("remember", params); assert.equal(retry.state, "committed");
      if (retry.state === "committed") { assert.equal(retry.id, after[i].id); assert.equal(retry.created, false); assert.equal(retry.ingest_seq, i + 1); }
      assert.equal((await c.request("ingest.status", { ...identity(receipts[i]!), body_digest: "0".repeat(64) })).state, "unknown");
      await assert.rejects(c.request("remember", { ...params, episode: { ...params.episode, content: "changed body" } }), { code: "revision_conflict" });
    }
    assert.deepEqual(await rows(), after);
    const retryEdges = await edges(); edgeOracle(retryEdges, after); assert.deepEqual(retryEdges, recoveredEdges);
    const meta = await metaSequence();
    assert.equal(meta, 2); assert.equal(recovered.outbox_pending, 2);
    record("topology-recovery-green.json", { sessionKey, rows: after, recoveredEdges, retryEdges, meta });
    record("recovery-green.json", { recovered, rows: after, meta, retainedReceiptHashes: await hashes((await readdir(join(runtimeRoot, "deliveries"))).map(f => join(runtimeRoot, "deliveries", f))) });
    // Transport-only response loss: consume a complete REAL daemon response,
    // withhold it from the client, and kill the daemon after that exact event.
    const proxyEvents = new EventEmitter();
    let proxyRoot = runtimeRoot;
    proxy = createServer(downstream => {
      track(downstream); const upstream = track(connect(join(proxyRoot, "anamnesis.sock")));
      let buffer = Buffer.alloc(0), requests = Buffer.alloc(0);
      const withheldIds = new Set<number>();
      // Observe request IDs before forwarding bytes. Error responses do not
      // have method fields; only the matching request determines withholding.
      downstream.on("data", (chunk: Buffer) => {
        requests = Buffer.concat([requests, chunk]);
        while (requests.length >= 4 && requests.length >= 4 + requests.readUInt32BE()) {
          const length = 4 + requests.readUInt32BE();
          const request = JSON.parse(requests.subarray(4, length).toString()); requests = requests.subarray(length);
          if (request.method === "remember") { withheldIds.add(request.id); log("proxy-requests.jsonl", { id: request.id, method: request.method, params: request.params }); }
        }
        upstream.write(chunk);
      });
      downstream.on("error", e => { log("proxy-errors.jsonl", String(e)); upstream.destroy(); });
      upstream.on("error", e => { log("proxy-errors.jsonl", String(e)); downstream.destroy(); });
      downstream.on("close", () => upstream.destroy()); upstream.on("close", () => downstream.destroy());
      upstream.on("data", (chunk: Buffer) => {
        buffer = Buffer.concat([buffer, chunk]);
        while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
          const length = 4 + buffer.readUInt32BE(); const frame = buffer.subarray(0, length); buffer = buffer.subarray(length);
          const response = JSON.parse(frame.subarray(4).toString());
          if (withheldIds.delete(response.id)) { log("proxy-withheld.jsonl", response); proxyEvents.emit("withheld", response); } else downstream.write(frame);
        }
      });
    });
    const proxyPath = join(runtimeRoot, "loss.sock"), proxyListening = once(proxy, "listening", { signal: deadline() }); proxy.listen(proxyPath); await proxyListening;
    const pc = await RpcClient.connect(proxyPath, token); clients.add(pc);
    const withheld = once(proxyEvents, "withheld", { signal: deadline() });
    const lostParams = input("response-loss", "20");
    const lost = pc.request("remember", lostParams).then(value => ({ success: true as const, value }), unknownOutcome);
    const [reply] = await withheld; assert.equal(reply.result.state, "committed"); assert.equal(reply.result.ingest_seq, 3);
    const lossRows = await rows(), lossEdges = await edges(); edgeOracle(lossEdges, lossRows);
    assert.equal((await daemon.stop()).signal, "SIGKILL");
    const lossOutcome = await lost; assert.equal(lossOutcome.success, false); if (!lossOutcome.success) assert.equal(lossOutcome.code, "outcome_unknown");
    await pc.close(); clients.delete(pc); await c.close(); clients.delete(c);
    daemon = await start(runtimeRoot); c = await client(runtimeRoot);
    const recoveredLoss = await c.request("ingest.status", identity(reply.result)); assert.equal(recoveredLoss.state, "committed");
    if (recoveredLoss.state === "committed") { assert.equal(recoveredLoss.id, reply.result.id); assert.equal(recoveredLoss.ingest_seq, 3); }
    const finalRows = await rows(); assert.equal(finalRows.length, 3); verifyRow(finalRows[2], lostParams, reply.result, 3);
    assert.deepEqual(finalRows, lossRows); assert.deepEqual(finalRows.slice(0, 2), after);
    const finalEdges = await edges(); edgeOracle(finalEdges, finalRows); assert.deepEqual(finalEdges, lossEdges);
    const lossRetry = await c.request("remember", lostParams); assert.deepEqual(lossRetry, { ...reply.result, created: false });
    assert.deepEqual(await rows(), finalRows); assert.deepEqual(await edges(), finalEdges); assert.equal(await metaSequence(), 3);
    record("response-loss.json", { boundary: "real response consumed, not forwarded; NOT commit-before-done", reply, recoveredLoss, client: lossOutcome, lossRows, lossEdges, finalRows, finalEdges, lossRetry, meta: await metaSequence() });
    await stopGracefully(daemon, c, runtimeRoot);
    // The topology case is finished. Reuse only this owned container with an
    // empty DB for LI's pinned ingest_seq=1; no shared database is touched.
    const cleared = await db.executeQuery("MATCH (n) DETACH DELETE n");
    record("legacy-isolation.json", { precedingCase: "topology complete and daemon stopped", updates: cleared.summary.counters.updates() });
    const legacyRoot = await mkdtemp("/tmp/ana-g001-legacy-"); roots.push(legacyRoot); log("resources.jsonl", { root: legacyRoot, owner });
    const rawRoot = join(legacyRoot, "historical-raw"); await mkdir(rawRoot);
    for (const [i, line] of legacyLines.entries()) {
      assert.equal(sha(line), legacyHashes[i]);
      writeFileSync(join(rawRoot, `F${i + 1}.jsonl`), line);
      const element = JSON.parse(line).element;
      await db.executeQuery(`CREATE (e:Element {id:$id,schema:$schema,content:$content,mass:$mass,properties:$properties,
        origin_source:$source,origin_session:$session,origin_actor:$actor,origin_record:$record,
        time_value:$time,time_precision:$precision,digest:$digest})`, {
        id: legacyIds[i], schema: element.schema, content: element.content, mass: element.mass,
        properties: JSON.stringify(element.properties), ...element.origin,
        time: element.time?.value ?? null, precision: element.time?.precision ?? null, digest: legacyDigests[i],
      });
    }
    await db.executeQuery(`MATCH (e:Element {id:$id}) SET e:Episode,e.origin_key=$origin,e.revision_key=$revision,e.ingest_seq=1
      CREATE (:OriginHead {origin_key:$origin,revision_key:$revision})
      CREATE (:Outbox {element_id:$id,enqueued_at:'historical-fixture'})-[:OF]->(e)
      CREATE (:Meta {key:'meta',ingest_seq:1})`, { id: legacyIds[0], origin: "324eaac8861d8ce9d127fe7895cfa5c0c8ed20c6c6907ff97d42cfe2e07b6531", revision: "3338e8315ea8e487094db0da6b32efa6f9a0619fc4a6318b848f55f9018430bf" });
    await db.executeQuery("MATCH (e:Element) WHERE e.id IN $ids SET e:Fact", { ids: legacyIds.slice(1) });
    const options = { uri: `bolt://127.0.0.1:${target}`, user: "neo4j", password, objectsRoot: join(legacyRoot, "objects") };
    const observed = neo4j.driver(options.uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
    const summaries: { query: string; updates: boolean; systemUpdates: boolean }[] = [];
    const execute = observed.executeQuery.bind(observed);
    observed.executeQuery = async function<T = EagerResult>(...args: Parameters<typeof observed.executeQuery<T>>) {
      const result = await execute<T>(...args);
      assert.ok(result instanceof EagerResult);
      summaries.push({ query: String(args[0]), updates: result.summary.counters.containsUpdates(), systemUpdates: result.summary.counters.containsSystemUpdates() });
      return result;
    };
    legacyStore = new Store(options, observed);
    const validId = "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e12";
    const legacyParams: RpcRememberParams = { episode: { schema: "anamnesis.original-message/1", content: "valid legacy", time: { value: "2026-09-02T10:00:00Z", precision: "second" }, origin: { source: "fixture", session: "valid", actor: "user", record: "r" }, mass: 0.5, properties: { z: 1, a: 2 } }, source_revision: "r", expected_previous_revision_key: null };
    assert.deepEqual(await legacyStore.putElement({ ...legacyParams.episode, id: validId }, { sourceRevision: "r", expectedPreviousRevisionKey: null, enqueue: true }), { id: validId, created: true });
    const frozenDigest = sha(JSON.stringify({ schema: legacyParams.episode.schema, content: legacyParams.episode.content, properties: legacyParams.episode.properties, time: legacyParams.episode.time, payload_hash: null, previous_revision_key: null }));
    await db.executeQuery("MATCH (e:Element {id:$id}) SET e.digest=$digest,e.episode_digest_version=1 REMOVE e.digest_format", { id: validId, digest: frozenDigest });
    async function legacySnapshot(directory = rawRoot) {
      const nodes = (await db!.executeQuery("MATCH (n) RETURN elementId(n) AS key,labels(n) AS labels,properties(n) AS props ORDER BY key")).records.map(r => {
        const row = r.toObject(); row["labels"].sort();
        // The only excluded field is legitimate writer fencing on Meta.
        if (row["labels"].includes("Meta") && row["props"].key === "meta") delete row["props"].writer_epoch;
        return row;
      });
      const relationships = (await db!.executeQuery("MATCH (a)-[r]->(b) RETURN elementId(r) AS key,elementId(a) AS a,elementId(b) AS b,type(r) AS type,properties(r) AS props ORDER BY key")).records.map(r => r.toObject());
      const raw = await Promise.all((await readdir(directory)).sort().map(async name => { const bytes = await readFile(join(directory, name)); return { name, base64: bytes.toString("base64"), sha256: sha(bytes) }; }));
      return { nodes, relationships, raw };
    }
    const legacyBefore = await legacySnapshot(); record("legacy-before.json", { params: legacyParams, validId, frozenDigest, snapshot: legacyBefore });
    const preservationOracle = (actual: Awaited<ReturnType<typeof legacySnapshot>>) => assert.deepEqual(actual, legacyBefore, "exact legacy raw bytes/hashes and graph preservation (except Meta.writer_epoch)");
    const copiedRaw = join(legacyRoot, "mutation-raw"); await cp(rawRoot, copiedRaw, { recursive: true });
    appendFileSync(join(copiedRaw, "F1.jsonl"), "fixture-only corruption\n");
    let legacyFailure: unknown;
    try { preservationOracle(await legacySnapshot(copiedRaw)); } catch (error) { legacyFailure = error; }
    assert.ok(legacyFailure instanceof assert.AssertionError);
    record("legacy-mutation-red.json", { mutation: "append bytes to copied F1 raw file", actual: legacyFailure.actual, expected: legacyFailure.expected, error: String(legacyFailure) });
    preservationOracle(await legacySnapshot());
    const inspectDirectory = join(legacyRoot, "inspection"); await mkdir(inspectDirectory);
    writeFileSync(join(inspectDirectory, "journal-2026-09.jsonl"), legacyLines.join(""));
    const inspected = await new EpisodeJournal(inspectDirectory).inspect("post167-pre194");
    assert.equal(inspected.length, 3); let offset = 0;
    for (const [i, item] of inspected.entries()) {
      assert.deepEqual(item.raw, Buffer.from(legacyLines[i]!)); assert.equal(item.sha256, legacyHashes[i]); assert.equal(item.offset, offset);
      assert.deepEqual(item.eligibility, [i === 2 ? "invalid-sub-kind" : "missing-time"]); offset += item.raw.length;
    }
    record("legacy-inspection.json", inspected);
    async function verifyLegacyReadOnly() {
      const beforeVerify = await legacySnapshot(); summaries.length = 0;
      const issues = await legacyStore!.verify();
      assert.ok(summaries.length > 0); assert.ok(summaries.every(summary => !summary.updates && !summary.systemUpdates));
      assert.deepEqual(await legacySnapshot(), beforeVerify); preservationOracle(beforeVerify);
      const expectedIssues = [
        ...legacyIds.map((elementId, i) => ({ elementId, kind: "semantic-ineligibility", reasons: [i === 2 ? "invalid-sub-kind" : "missing-time"] })),
        { elementId: legacyIds[0], kind: "unsupported-topology-format" },
        { elementId: validId, kind: "unsupported-digest-format" },
      ];
      const sorted = (items: unknown[]) => items.map(item => canonical(item)).sort();
      assert.deepEqual(sorted(issues), sorted(expectedIssues)); // No digest or valid-time legacy issue.
      return { summaries: [...summaries], issues, snapshot: beforeVerify };
    }
    record("legacy-verify-before.json", await verifyLegacyReadOnly());
    proxyRoot = legacyRoot; daemon = await start(legacyRoot); c = await client(legacyRoot);
    preservationOracle(await legacySnapshot());
    const lc = await RpcClient.connect(proxyPath, token); clients.add(lc);
    const withheldError = once(proxyEvents, "withheld", { signal: deadline() });
    const legacyLost = lc.request("remember", legacyParams).then(value => ({ success: true as const, value }), unknownOutcome);
    const [refusal] = await withheldError;
    assert.equal(refusal.error.data.code, "unsupported_digest_version"); assert.equal(refusal.error.data.retryable, false);
    assert.equal("method" in refusal, false); // ID matcher must handle a real error envelope.
    preservationOracle(await legacySnapshot());
    assert.equal((await daemon.stop()).signal, "SIGKILL");
    const legacyOutcome = await legacyLost; assert.equal(legacyOutcome.success, false); if (!legacyOutcome.success) assert.equal(legacyOutcome.code, "outcome_unknown");
    await lc.close(); clients.delete(lc); await c.close(); clients.delete(c);
    preservationOracle(await legacySnapshot());
    daemon = await start(legacyRoot); c = await client(legacyRoot);
    const repeatedRefusal = await c.request("remember", legacyParams).then(value => ({ success: true as const, value }), unknownOutcome);
    assert.equal(repeatedRefusal.success, false); if (!repeatedRefusal.success) assert.equal(repeatedRefusal.code, "unsupported_digest_version");
    const legacyAfter = await legacySnapshot(); preservationOracle(legacyAfter); assert.equal(await metaSequence(), 2);
    record("legacy-response-loss.json", { boundary: "real error matched by request ID and withheld before SIGKILL", refusal, client: legacyOutcome, repeatedRefusal, snapshot: legacyAfter, meta: await metaSequence() });
    record("legacy-verify-after.json", await verifyLegacyReadOnly());
    await stopGracefully(daemon, c, legacyRoot);
    preservationOracle(await legacySnapshot());
    const finalHashes = await hashes(files); record("source-after.json", finalHashes); assert.deepEqual(finalHashes, before, "source changed during verification");
  } catch (error) { failure = error; record("result.json", { ok: false, error: String(error), stack: error instanceof Error ? error.stack : null }); }
  finally {
    const errors: unknown[] = [];
    const clean = async (label: string, action: () => Promise<void>) => { try { await action(); cleanup.push({ label, ok: true }); } catch (error) { errors.push(error); cleanup.push({ label, error: String(error) }); } record("cleanup.json", cleanup); };
    for (const c of clients) await clean("client", () => c.close());
    for (const p of children.filter(p => p.child.spawnargs[0] !== "docker")) await clean(`child ${p.child.pid}`, async () => { await p.stop(); });
    if (legacyStore) await clean("legacy Store driver", () => legacyStore!.close());
    if (db) await clean("driver", () => db!.close());
    if (attempted) await clean("owned container", async () => {
      const inspected = JSON.parse(await docker(["inspect", "--format", "{{json .}}", name]));
      assert.equal(inspected.Config.Labels["anamnesis.qa.owner"], owner);
      await docker(["rm", "-f", "-v", inspected.Id]); cleanup.push({ container: inspected.Id, owner });
      const remains = await docker(["ps", "-a", "-q", "--filter", `label=anamnesis.qa.owner=${owner}`]); assert.equal(remains, "");
    });
    for (const p of children) await clean(`terminal ${p.child.pid}`, async () => { await p.stop(); });
    for (const s of sockets) s.destroy();
    for (const server of [proxy, relay]) if (server?.listening) await clean("listener", async () => { const closed = once(server, "close", { signal: deadline() }); server.close(); await closed; });
    for (const r of roots) await clean(r, async () => { await rm(r, { recursive: true, force: true }); await absent(r); });
    if (errors.length) failure = new AggregateError([...(failure ? [failure] : []), ...errors], "verification/cleanup failure");
  }
  record("result.json", failure ? { ok: false, error: String(failure), stack: failure instanceof Error ? failure.stack : null } : { ok: true });
  if (failure) throw failure;
});
