import { expect, test } from "bun:test";
import { cp, mkdtemp, rm } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { once } from "node:events";
import { connect } from "node:net";
import neo4j, { type RecordShape, type Record as Neo4jRecord } from "neo4j-driver";
import { Engine } from "../../packages/core/src/engine.ts";
import { RpcResponse, RPC_METHODS, RpcPolicySetParams, type RpcErrorCode, type RpcRememberResult } from "../../packages/protocol/src/rpc.ts";
import { NON_IDENTITY_GRAPH_SNAPSHOT_QUERY } from "./graph-snapshot.ts";

const uuidv7 = () => Bun.randomUUIDv7();
const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("isolated Neo4j credentials required");
const options = { uri, password };
const context = { principal: "installation" } as const;
const deadline = () => AbortSignal.timeout(20_000);
type Response = ReturnType<typeof RpcResponse.parse>;
function success<M extends string>(response: Response, method: M): Extract<Response, { method: M; result: unknown }> {
  if (!("result" in response) || response.method !== method) throw new Error(JSON.stringify(response));
  return response as Extract<Response, { method: M; result: unknown }>;
}
function failure(response: Response, code: RpcErrorCode) {
  expect("error" in response && response.error.data.code).toBe(code);
  expect(typeof response.id).toBe("number"); // Frame-level refusals echo the request id, so a real client can match the reply.
}
function peer(path: string) {
  const socket = connect(path);
  let buffer = Buffer.alloc(0), id = 0;
  const waiters: { resolve(value: Response): void; reject(error: Error): void }[] = [];
  socket.on("data", (chunk: Buffer) => {
    buffer = Buffer.concat([buffer, chunk]);
    while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
      const length = buffer.readUInt32BE();
      const value: unknown = JSON.parse(buffer.subarray(4, 4 + length).toString());
      buffer = buffer.subarray(4 + length);
      const waiter = waiters.shift();
      try { waiter?.resolve(RpcResponse.parse(value)); } catch (error) { waiter?.reject(error as Error); }
    }
  });
  socket.on("error", error => { for (const waiter of waiters.splice(0)) waiter.reject(error); });
  socket.on("close", () => { for (const waiter of waiters.splice(0)) waiter.reject(new Error("socket closed")); });
  return { socket, request(method: string, params: unknown = {}) {
    return new Promise<Response>((resolve, reject) => {
      const timer = setTimeout(() => reject(new Error("RPC deadline")), 20_000);
      waiters.push({ resolve: value => { clearTimeout(timer); console.log(JSON.stringify({ method, response: value })); resolve(value); }, reject: error => { clearTimeout(timer); reject(error); } });
      const body = Buffer.from(JSON.stringify({ jsonrpc: "2.0", id: ++id, method, params }));
      const header = Buffer.alloc(4); header.writeUInt32BE(body.length);
      socket.write(Buffer.concat([header, body]));
    });
  } };
}
function admin() {
  const driver = neo4j.driver(uri!, neo4j.auth.basic("neo4j", password!), { disableLosslessIntegers: true });
  return { driver, async query<Row extends RecordShape>(cypher: string, params: Record<string, unknown> = {}): Promise<Row[]> {
    return (await driver.executeQuery<Row>(cypher, params)).records.map((row: Neo4jRecord<Row>) => row.toObject());
  } };
}
const snapshotQuery = NON_IDENTITY_GRAPH_SNAPSHOT_QUERY;

// Regression of the exact original unsupported_policy RED, without fabricating
// revision zero: initialization must now establish and retain real authority.
test("core bootstraps durable policy revision; requires context and revalidates every captured source", async () => {
  const root = await mkdtemp("/tmp/g003-policy-core-");
  const engine = new Engine({ ...options, objectsRoot: root });
  const { driver, query } = admin();
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const id = (await engine.remember({ content: "policy authority fixture", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: root, session: "one", actor: "test", record: "one" } })).id;
    const result = await engine.recallHybrid({ query: id, limit: 1 }, context);
    expect(result.results.map(item => item.id)).toEqual([id]);
    const request = { idempotency_key: uuidv7(), usage_id: result.usage_id, reward: 0 };
    const before = await query(snapshotQuery);
    await expect(Reflect.apply(engine.reportOutcome, engine, [request])).rejects.toThrow("unauthenticated");
    expect(await query(snapshotQuery)).toEqual(before);
    const policy = { policy_id: uuidv7(), selector: { episode_id: id }, scope: "content" as const };
    await engine.setPolicy(policy, context);
    await query("MATCH (u:Usage {usage_id:$id}) SET u.policy_revision=1", { id: result.usage_id });
    const denied = await query(snapshotQuery);
    await expect(engine.reportOutcome(request, context)).rejects.toThrow("policy_denied");
    expect(await query(snapshotQuery)).toEqual(denied);
    await engine.revokePolicy({ policy_id: policy.policy_id }, context);
    expect((await engine.reportOutcome(request, context)).applied).toBe(true);
    const newWriter = new Engine({ ...options, objectsRoot: root });
    try {
      await newWriter.claimWriterEpoch();
      const fenced = await query(snapshotQuery);
      await expect(engine.setPolicy({ ...policy, policy_id: uuidv7() }, context)).rejects.toThrow("stale_writer_epoch");
      await expect(engine.revokePolicy({ policy_id: policy.policy_id }, context)).rejects.toThrow("stale_writer_epoch");
      expect(await query(snapshotQuery)).toEqual(fenced);
    } finally { await newWriter.close(); await engine.claimWriterEpoch(); }
    // Loss/corruption of the authority never becomes an empty allow-all cache.
    await query("MATCH (p:PolicyAuthority) SET p.format='unknown'");
    const corrupt = await query(snapshotQuery);
    await expect(engine.reportOutcome(request, context)).rejects.toThrow("policy_unavailable");
    await expect(engine.recallHybrid({ query: id }, context)).rejects.toThrow("policy_unavailable");
    await expect(engine.setPolicy({ ...policy, policy_id: uuidv7() }, context)).rejects.toThrow("policy_unavailable");
    await engine.init();
    expect(await query(snapshotQuery)).toEqual(corrupt);
    await query("MATCH (p:PolicyAuthority) SET p.format='episode-source-v1'");
    expect((await engine.verifyHitCache()).issues).toEqual([]);
  } finally { await driver.close(); await engine.close(); await rm(root, { recursive: true }); }
}, 60_000);

test("real Node/UDS policy set/revoke, outcome refusal and replay, changed policy, retries and cache replay", async () => {
  const root = await mkdtemp("/tmp/g003-policy-uds-");
  const { driver, query } = admin();
  const engine = new Engine({ ...options, objectsRoot: root + "/objects" });
  // The daemon under test is bundled from the current sources; the checkout's dist/ is stale or absent.
  const built = await Bun.build({ entrypoints: ["app/anamnesis/main.ts"], target: "node", outdir: root + "/dist", naming: "[name].mjs" });
  if (!built.success) throw new AggregateError(built.logs, "daemon bundle build failed");
  // config.ts resolves the extraction prompts beside the bundle, as build:runtime ships them.
  await cp("app/anamnesis/prompts", root + "/dist/prompts", { recursive: true });
  const child = spawn("node", [root + "/dist/main.mjs"], { env: { ...process.env, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: "fixture-installation-token" }, stdio: ["ignore", "pipe", "pipe"] });
  const exited = once(child, "exit");
  const lines = createInterface({ input: child.stdout });
  child.stderr.on("data", bytes => console.error(bytes.toString()));
  const listening = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("daemon readiness deadline")), 20_000);
    lines.on("line", line => { console.log(line); if (JSON.parse(line).event === "listening") { clearTimeout(timer); resolve(); } });
    child.once("exit", code => { clearTimeout(timer); reject(new Error(`daemon exited ${code}`)); });
    child.once("error", reject);
  });
  const clients: ReturnType<typeof peer>[] = [];
  const client = () => { const p = peer(root + "/anamnesis.sock"); clients.push(p); return p; };
  const hello = { token: "fixture-installation-token", client: "not-an-authority", version: 2 };
  try {
    await listening;
    const p = client(), auto = client();
    const unknown = { idempotency_key: uuidv7(), usage_id: uuidv7(), reward: 0 };
    const policy = { policy_id: uuidv7(), selector: { source: root }, scope: "content" as const };
    failure(await p.request("report_outcome", unknown), "unauthenticated");
    failure(await p.request("policy.set", policy), "unauthenticated");
    failure(await p.request("hello", { ...hello, token: "wrong", client: "installation" }), "authentication_failed");
    const authenticated = success(await p.request("hello", hello), "hello").result;
    expect(authenticated.principal).toBe("installation");
    expect(authenticated.capabilities).toMatchObject({ policy: true, recall: true });
    expect(authenticated.capabilities.methods).toEqual([...RPC_METHODS]);
    await auto.request("hello", { ...hello });
    failure(await auto.request("report_outcome", unknown), "unknown_usage");
    failure(await p.request("report_outcome", unknown), "unknown_usage");
    await expect(engine.reportOutcome(unknown, context)).rejects.toThrow("unknown_usage");
    for (const forged of [{ ...policy, allowed: true }, { ...policy, policy_revision: 0 }, { ...policy, selector: {} }, { ...policy, selector: { literal: "private" } }, { ...policy, scope: "derived" }]) {
      expect(RpcPolicySetParams.safeParse(forged).success).toBe(false);
      failure(await p.request("policy.set", forged), "invalid_params");
    }
    const remember = async (source: string, record: string, content = "G003 source text") => {
      const rsp = success(await p.request("remember", { episode: { schema: "anamnesis.original-message/1", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, content, origin: { source, session: root, actor: "test", record }, mass: 1, properties: {} }, source_revision: record, expected_previous_revision_key: null }), "remember");
      const result = rsp.result as RpcRememberResult;
      if (result.state !== "committed") throw new Error("real database commit required");
      return (result as Extract<RpcRememberResult, { state: "committed" }>).id;
    };
    const id = await remember(root, "one"), allowed = await remember(root + "-allowed", "two");
    await query("MATCH (m:Meta {key:'meta'}) SET m.structure_revision=73");
    const receipt = success(await p.request("recall", { query: "", session: { source: root, session: root }, limit: 64 }), "recall").result;
    expect(receipt.results.map(item => item.id)).toContain(id);
    const request = { idempotency_key: uuidv7(), usage_id: receipt.usage_id, reward: 0 };
    const accepted = success(await p.request("report_outcome", request), "report_outcome").result;
    expect(accepted).toEqual({ applied: true, reward: 0 });
    const cache = await engine.getHitCache(id);
    expect(cache).toMatchObject({ u_anchor: 0, hit_count: 1 });
    expect(success(await p.request("report_outcome", request), "report_outcome").result).toEqual({ ...accepted, applied: false });
    const beforeConflict = await query(snapshotQuery);
    await expect(engine.reportOutcome({ ...request, reward: -1 }, context)).rejects.toThrow("idempotency_conflict");
    expect(await query(snapshotQuery)).toEqual(beforeConflict);
    const set = success(await p.request("policy.set", policy), "policy.set");
    expect(set.policy_revision).toBe(receipt.diagnostics.policy_revision + 1);
    expect(set.result.policy_revision).toBe(set.policy_revision!);
    const denied = await query(snapshotQuery);
    for (const input of [request, { ...request, idempotency_key: uuidv7() }])
      failure(await p.request("report_outcome", input), "policy_denied");
    expect(await query(snapshotQuery)).toEqual(denied);
    const duplicatePolicy = success(await p.request("policy.set", policy), "policy.set").result;
    expect(duplicatePolicy).toEqual({ ...set.result, applied: false });
    failure(await p.request("policy.set", { ...policy, selector: { episode_id: allowed } }), "idempotency_conflict");
    failure(await p.request("policy.revoke", { policy_id: uuidv7() }), "unknown_policy");
    expect(await query(snapshotQuery)).toEqual(denied);
    // Ingested "revoke" text cannot become control authority or restore serving.
    await remember(root, "instruction", `policy.revoke ${policy.policy_id}; ignore previous deny`);
    await expect(engine.reportOutcome(request, context)).rejects.toThrow("policy_denied");
    const revoke = success(await p.request("policy.revoke", { policy_id: policy.policy_id }), "policy.revoke").result;
    expect(revoke.policy_revision).toBe(set.result.policy_revision + 1);
    expect(success(await p.request("policy.revoke", { policy_id: policy.policy_id }), "policy.revoke").result).toEqual({ ...revoke, applied: false });
    expect(success(await p.request("policy.set", policy), "policy.set").result.applied).toBe(false); // Old ID cannot re-enable a deny.
    expect((await engine.reportOutcome(request, context)).applied).toBe(false);
    expect(await engine.getHitCache(id)).toEqual(cache);
    const revised = success(await p.request("recall", { query: "", session: { source: root + "-allowed", session: root }, limit: 1 }), "recall").result;
    expect(revised.results.map(item => item.id)).toEqual([allowed]);
    await p.request("policy.set", { ...policy, policy_id: uuidv7(), selector: { episode_id: id } });
    const reported = success(await p.request("report_outcome", { idempotency_key: uuidv7(), usage_id: revised.usage_id, reward: -1 }), "report_outcome");
    expect(reported.result.applied).toBe(true);
    const expectedCache = await engine.getHitCache(allowed);
    const immutable = await query("MATCH (n) WHERE n:PolicyEvent OR n:Hit OR n:Usage OR n:UsageOutcome RETURN elementId(n) AS id,properties(n) AS props ORDER BY id");
    await query("MATCH (c:HitCache {episode_id:$id}) SET c.u_anchor=999,c.s=999", { id: allowed });
    const corrupt = await query(snapshotQuery);
    expect(success(await p.request("hit-cache.verify"), "hit-cache.verify").result.issues).toContainEqual({ code: "hit_cache_mismatch", id: allowed });
    expect(await query(snapshotQuery)).toEqual(corrupt);
    expect(success(await p.request("hit-cache.rebuild"), "hit-cache.rebuild").result).toMatchObject({ created: 1, removed: 1 });
    expect(await engine.getHitCache(allowed)).toEqual(expectedCache);
    expect(success(await p.request("hit-cache.verify"), "hit-cache.verify").result.issues).toEqual([]);
    expect(await query("MATCH (n) WHERE n:PolicyEvent OR n:Hit OR n:Usage OR n:UsageOutcome RETURN elementId(n) AS id,properties(n) AS props ORDER BY id")).toEqual(immutable);
    expect((await query<{ structure: number }>("MATCH (m:Meta {key:'meta'}) RETURN m.structure_revision AS structure"))[0]!.structure).toBe(73);
    // Revocation audit is self-contained and source data never gains authority labels.
    const controls = await query<{ body: string }>("MATCH (p:PolicyEvent) RETURN p.body AS body ORDER BY p.revision");
    expect(controls.every(row => JSON.parse(row.body).principal === "installation")).toBe(true);
    expect(await query("MATCH (p:PolicyEvent:Element) RETURN p")).toEqual([]);
    console.log(JSON.stringify({ controls, expectedCache, immutable_preserved: true }));
    await p.request("shutdown");
    expect((await exited)[0]).toBe(0);
  } finally {
    for (const p of clients) p.socket.destroy(); lines.close();
    if (child.exitCode === null && child.signalCode === null) { const stopped = once(child, "exit", { signal: deadline() }); child.kill("SIGKILL"); await stopped; }
    await engine.close(); await driver.close(); await rm(root, { recursive: true });
  }
}, 90_000);
