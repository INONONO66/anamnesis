import { expect, test } from "bun:test";
import { once } from "node:events";
import { appendFileSync } from "node:fs";
import { createServer, connect, type Socket } from "node:net";
import { mkdir, mkdtemp, readFile, writeFile, access, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import neo4j from "neo4j-driver";
import { createContinuousDriver } from "./g006-continuous-driver.ts";
import { runContinuousSoak } from "./g006-runtime-scenarios.ts";
import { startProcess } from "./runtime-scenarios.ts";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { Frames, encode } from "../../app/anamnesis/wire.ts";

// Run only through the existing owned runner, which injects fresh private DB credentials.
test("real Node daemon: verified committed artifacts, lost UDS reply, death and unavailable storage remain distinct", async () => {
  const uri = process.env.ANAMNESIS_TEST_NEO4J_URI, password = process.env.ANAMNESIS_TEST_NEO4J_PASSWORD;
  if (!uri || !password) throw new Error("owned runner credentials required");
  const endpoint = { uri, user: process.env.ANAMNESIS_TEST_NEO4J_USER ?? "neo4j", password };
  const base = resolve(".omo/evidence/g006-lifecycle-repair/surface");
  await mkdir(base, { recursive: true });
  const evidence = await mkdtemp(join(base, "node-"));
  await mkdir(join(evidence, "dist"));
  const built = await startProcess(process.execPath, ["build", "app/anamnesis/main.ts", "--target=node", "--outfile", join(evidence, "dist/anamnesis-daemon.mjs")], { deadlineMs: 120_000 }).done;
  await writeFile(join(evidence, "build.json"), JSON.stringify(built, null, 2));
  expect(built.code).toBe(0);
  const db = neo4j.driver(uri, neo4j.auth.basic(endpoint.user, password));
  const json = async (dir: string, file: string) => JSON.parse(await readFile(join(dir, file), "utf8"));
  const events = async (dir: string) => (await readFile(join(dir, "continuous-events.jsonl"), "utf8")).trim().split("\n").map(line => JSON.parse(line));
  const scenario = async (name: string) => { const dir = join(evidence, name); await mkdir(dir); return dir; };
  const absent = async (path: string) => expect(await access(path).then(() => false, error => { if (error.code !== "ENOENT") throw error; return true; })).toBe(true);
  try {
    await db.verifyConnectivity();
    await db.executeQuery("MATCH (n) DETACH DELETE n");
    const healthy = await scenario("healthy");
    const driver = createContinuousDriver(startProcess, healthy, evidence, new AbortController().signal, endpoint);
    try {
      await driver.launchOwnedDaemon(); await driver.subscribe();
      await driver.traffic(0); await driver.traffic(1); await driver.recover(1); await driver.terminal();
    } finally { await driver.cleanup(); }
    const healthyEvents = await events(healthy);
    const committed = healthyEvents.filter(e => e.event === "traffic_committed");
    expect(committed).toHaveLength(2);
    const statuses = healthyEvents.filter(e => e.event === "status");
    expect(new Set(statuses.map(e => e.status.fs_epoch)).size).toBe(1);
    expect(new Set(healthyEvents.map(e => e.pid).filter(Boolean)).size).toBe(1);
    const terminal = healthyEvents.find(e => e.event === "terminal_boundary");
    expect(terminal.fs_epoch).toBe(statuses[0].status.fs_epoch);
    for (const event of committed) {
      const rows = await db.executeQuery("MATCH (e:Element:Episode {revision_key:$key}) RETURN e.id AS id, e.content AS content, e.ingest_seq AS seq", { key: event.receipt.revision_key });
      expect(rows.records).toHaveLength(1);
      expect(rows.records[0]!.get("id")).toBe(event.receipt.id);
      expect(rows.records[0]!.get("content")).toBe(`G006 continuous traffic ${event.sequence}`);
      expect(Number(rows.records[0]!.get("seq"))).toBe(event.receipt.ingest_seq);
    }
    const timingRows = async (dir: string, file: string) => (await readFile(join(dir, file), "utf8")).trim().split("\n").map(line => JSON.parse(line));
    const clientTiming = await timingRows(healthy, "continuous-timing.jsonl");
    const daemonTiming = await timingRows(healthy, "continuous-daemon-timing.jsonl");
    for (const request of clientTiming.filter(row => row.layer === "client" && row.event === "request" && ["status", "remember", "ingest.status"].includes(row.method))) {
      expect(request.deadlineMs).toBe(120_000);
      expect(clientTiming.find(row => row.layer === "client" && row.event === "response" && row.id === request.id)).toMatchObject({ method: request.method, elapsedMs: expect.any(Number) });
      const daemonRequest = daemonTiming.filter(row => row.id === request.id);
      for (const event of ["request_received", "dispatch_start", "dispatch_complete", "write_start", "local_complete"]) {
        expect(daemonRequest.find(row => row.event === event)).toMatchObject({ method: request.method, elapsedMs: expect.any(Number) });
      }
      expect(daemonRequest.some(row => row.layer === "runtime" && row.operation === "neo4j.read" && row.event === "start")).toBe(true);
      expect(daemonRequest.some(row => row.layer === "runtime" && row.operation === "neo4j.read" && row.event === "complete")).toBe(true);
    }
    expect(clientTiming.filter(row => row.layer === "client" && row.event === "request" && row.method === "remember")).toHaveLength(2);
    for (const rows of [clientTiming, daemonTiming]) {
      expect(rows.map(row => row.eventSequence)).toEqual(rows.map((_, index) => index + 1));
      expect(rows.every(row => typeof row.monotonicMs === "number" && typeof row.at === "string")).toBe(true);
      expect(JSON.stringify(rows)).not.toContain(password);
      expect(JSON.stringify(rows)).not.toContain("G006 continuous traffic");
    }
    const healthyCleanup = await json(healthy, "continuous-cleanup.json");
    expect(healthyCleanup.daemon).toMatchObject({ code: 0, signal: null, timedOut: false });
    expect(healthyCleanup.errors).toEqual([]); await absent(healthyCleanup.root);
    await db.executeQuery("MATCH (n) DETACH DELETE n");

    // The relay observes the real committed remember reply, then drops that reply
    // and closes the transport. This is not a mocked receipt or a timed disconnect.
    const broken = await scenario("lost-reply");
    const relayRoot = await mkdtemp("/tmp/g006-relay-");
    const relayPath = join(relayRoot, "relay.sock");
    const sockets = new Set<Socket>();
    let target = "", rememberCalls = 0;
    let lostReceipt: Record<string, unknown> | undefined;
    const trace = (value: object) => appendFileSync(join(broken, "relay-events.jsonl"), JSON.stringify(value) + "\n");
    const relay = createServer(downstream => {
      trace({ event: "connected" }); sockets.add(downstream);
      const upstream = connect(target); sockets.add(upstream);
      const methods = new Map<number, string>();
      const requests = new Frames(body => {
        const message = JSON.parse(body.toString()); methods.set(message.id, message.method);
        trace({ event: "request", id: message.id, method: message.method });
        if (message.method === "remember") rememberCalls++;
        return true;
      });
      const responses = new Frames(body => {
        const message = JSON.parse(body.toString());
        trace({ event: "response", id: message.id, method: message.method, state: message.result?.state });
        if (methods.get(message.id) === "remember") {
          lostReceipt = message.result; downstream.destroy(); upstream.destroy();
        } else downstream.write(encode(message));
        return true;
      });
      downstream.on("data", (bytes: Buffer) => { requests.push(bytes); upstream.write(bytes); });
      upstream.on("data", (bytes: Buffer) => responses.push(bytes));
      downstream.on("error", error => { throw error; }); upstream.on("error", error => { throw error; });
    });
    const listening = once(relay, "listening", { signal: AbortSignal.timeout(5000) });
    relay.listen(relayPath); await listening;
    const originalConnect = RpcClient.connect;
    RpcClient.connect = (path, token, mode, timing) => { target = path; return originalConnect.call(RpcClient, relayPath, token, mode, timing); };
    try {
      const lost = await runContinuousSoak({ durationMs: 86_400_000, now: () => performance.now(), signal: new AbortController().signal,
        driver: createContinuousDriver(startProcess, broken, evidence, new AbortController().signal, endpoint),
        record: value => writeFile(join(broken, "result.json"), JSON.stringify(value, null, 2)) });
      expect(lost.status).toBe("UNKNOWN"); expect(lost.completedAt).toBeUndefined(); expect(lost.trafficCount).toBe(0);
      expect(rememberCalls).toBe(1); expect(lostReceipt?.state).toBe("committed");
      expect(lost.error).toContain("delivery outcome is unknown"); expect(lost.cleanupError).toContain("RPC connection is closed");
      const persisted = await db.executeQuery("MATCH (e:Episode {revision_key:$key}) RETURN count(e) AS count", { key: lostReceipt!.revision_key });
      expect(Number(persisted.records[0]!.get("count"))).toBe(1);
      await writeFile(join(broken, "relay-observation.json"), JSON.stringify({ rememberCalls, lostReceipt, actualCommittedArtifacts: 1 }, null, 2));
      const history = await events(broken);
      expect(history.some(e => e.event === "terminal_boundary")).toBe(false);
      expect(history.find(e => e.event === "rpc_failed" && e.method === "remember").code).toBe("outcome_unknown");
      expect(history.filter(e => e.event === "traffic_committed")).toHaveLength(0);
      const lostTiming = await timingRows(broken, "continuous-timing.jsonl");
      const request = lostTiming.find(row => row.layer === "client" && row.method === "remember" && row.event === "request");
      expect(lostTiming.filter(row => row.layer === "client" && row.method === "remember" && row.event === "request")).toHaveLength(1);
      expect(lostTiming.find(row => row.layer === "client" && row.id === request.id && row.event === "failed")).toMatchObject({ elapsedMs: expect.any(Number) });
      expect(lostTiming.some(row => row.layer === "client" && row.id === request.id && row.event === "response")).toBe(false);
      expect((await timingRows(broken, "continuous-daemon-timing.jsonl")).some(row => row.id === request.id && row.event === "dispatch_complete")).toBe(true);
      const cleanup = await json(broken, "continuous-cleanup.json");
      expect(cleanup.daemon).toMatchObject({ code: 1, signal: "SIGKILL", timedOut: false }); await absent(cleanup.root);
    } finally {
      RpcClient.connect = originalConnect;
      const closed = once(relay, "close", { signal: AbortSignal.timeout(5000) });
      relay.close(); for (const socket of sockets) socket.destroy(); await closed;
      await rm(relayRoot, { recursive: true, force: true });
    }

    const dead = await scenario("daemon-death");
    let processHandle: ReturnType<typeof startProcess> | undefined;
    const capture: typeof startProcess = (command, args, options) => processHandle = startProcess(command, args, options);
    const deathDriver = createContinuousDriver(capture, dead, evidence, new AbortController().signal, endpoint);
    const death = await runContinuousSoak({ durationMs: 86_400_000, now: () => performance.now(), signal: new AbortController().signal,
      driver: { ...deathDriver, subscribe: async () => { await deathDriver.subscribe(); processHandle!.stop(); await processHandle!.done; } },
      record: value => writeFile(join(dead, "result.json"), JSON.stringify(value, null, 2)) });
    expect(death.status).toBe("UNKNOWN"); expect(death.completedAt).toBeUndefined(); expect(death.trafficCount).toBe(0);
    const deadCleanup = await json(dead, "continuous-cleanup.json");
    expect(deadCleanup.daemon).toMatchObject({ code: 1, signal: "SIGKILL", timedOut: false }); await absent(deadCleanup.root);

    // A private refusing TCP endpoint, never the default/shared Neo4j, proves
    // that listening + hello is insufficient admission for soak traffic.
    const unavailable = await scenario("storage-unavailable");
    const refusing = createServer(socket => socket.destroy());
    const bound = once(refusing, "listening", { signal: AbortSignal.timeout(5000) });
    refusing.listen(0, "127.0.0.1"); await bound;
    try {
      const address = refusing.address(); if (!address || typeof address === "string") throw new Error("missing private port");
      const result = await runContinuousSoak({ durationMs: 86_400_000, now: () => performance.now(), signal: new AbortController().signal,
        driver: createContinuousDriver(startProcess, unavailable, evidence, new AbortController().signal, { ...endpoint, uri: `bolt://127.0.0.1:${address.port}` }),
        record: value => writeFile(join(unavailable, "result.json"), JSON.stringify(value, null, 2)) });
      expect(result.status).toBe("UNKNOWN"); expect(result.reason).toContain("continuous storage not ready: unavailable/degraded");
      expect(result.trafficCount).toBe(0); expect(result.completedAt).toBeUndefined();
      expect((await events(unavailable)).some(e => e.event === "traffic_admitted")).toBe(false);
      const cleanup = await json(unavailable, "continuous-cleanup.json"); expect(cleanup.daemon.code).toBe(0); await absent(cleanup.root);
    } finally { const closed = once(refusing, "close", { signal: AbortSignal.timeout(5000) }); refusing.close(); await closed; }
    console.log(JSON.stringify({ event: "g006-focused-surface-pass", evidence, qualification: "UNKNOWN", scenarios: ["healthy", "lost-reply", "daemon-death", "storage-unavailable"] }));
  } finally { await db.close(); }
}, 180_000);
