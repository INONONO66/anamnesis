import { expect, test } from "bun:test";
import { mkdtemp, rm } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { once } from "node:events";
import { connect } from "node:net";
import neo4j, { type RecordShape, type Record as Neo4jRecord } from "neo4j-driver";
import { Engine } from "../../packages/core/src/engine.ts";
import { RpcResponse, RPC_METHODS, RpcPolicySetParams, type RpcErrorCode, type RpcRememberResult } from "../../packages/protocol/src/rpc.ts";

const uuidv7 = () => Bun.randomUUIDv7();
const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
if (!uri || !password) throw new Error("isolated Neo4j credentials required");
const options = { uri, password };
const context = { principal: "installation", commit_mode: "receipt" } as const;
const deadline = () => AbortSignal.timeout(20_000);
type Response = ReturnType<typeof RpcResponse.parse>;
function success<M extends string>(response: Response, method: M): Extract<Response, { method: M; result: unknown }> {
  if (!("result" in response) || response.method !== method) throw new Error(JSON.stringify(response));
  return response as Extract<Response, { method: M; result: unknown }>;
}
function failure(response: Response, code: RpcErrorCode) {
  expect("error" in response && response.error.data.code).toBe(code);
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
const snapshotQuery = `MATCH (n) OPTIONAL MATCH (n)-[r]->(target)
  WITH n,r,target ORDER BY elementId(r)
  WITH n,collect({id:elementId(r),type:type(r),target:elementId(target),props:properties(r)}) AS edges
  RETURN elementId(n) AS id,labels(n) AS labels,properties(n) AS props,edges ORDER BY id`;

// Regression of the exact original unsupported_policy RED, without fabricating
// revision zero: initialization must now establish and retain real authority.
test("core bootstraps durable policy revision; requires context and revalidates every captured source", async () => {
  const root = await mkdtemp("/tmp/g003-policy-core-");
  const engine = new Engine({ ...options, objectsRoot: root });
  const { driver, query } = admin();
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const id = (await engine.remember({ content: "policy authority fixture", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: root, session: "one", actor: "test", record: "one" } })).id;
    const input = { recall_id: uuidv7(), primary_ids: [id] };
    console.log(JSON.stringify({ fixture: input }));
    const receipt = await engine.issueReceipt(input, context);
    expect(receipt.policy_revision).toBe(0);
    const request = { operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: [id], reward: 0 };
    const before = await query(snapshotQuery);
    await expect(Reflect.apply(engine.commitReceipt, engine, [request])).rejects.toThrow("unauthenticated");
    await expect(engine.commitReceipt(request, { ...context, commit_mode: "auto" })).rejects.toThrow("commit_mode_mismatch");
    expect(await query(snapshotQuery)).toEqual(before);
    const policy = { policy_id: uuidv7(), selector: { episode_id: id }, scope: "content" as const };
    await engine.setPolicy(policy, context);
    const denied = await query(snapshotQuery);
    await expect(engine.commitReceipt({ ...request, adopted: [] }, context)).rejects.toThrow("policy_denied");
    await expect(engine.issueReceipt(input, context)).rejects.toThrow("policy_denied");
    // A forged captured revision equal to the current revision is not permission.
    const body = (await query<{ body: string }>("MATCH (r:RecallReceipt {recall_id:$id}) RETURN r.body AS body", { id: receipt.recall_id }))[0]!.body;
    await query("MATCH (r:RecallReceipt {recall_id:$id}) SET r.body=$body", { id: receipt.recall_id, body: JSON.stringify({ ...JSON.parse(body), policy_revision: 1 }) });
    const unchanged = await query(snapshotQuery);
    await expect(engine.commitReceipt(request, context)).rejects.toThrow("policy_denied");
    expect(await query(snapshotQuery)).toEqual(unchanged);
    await query("MATCH (r:RecallReceipt {recall_id:$id}) SET r.body=$body", { id: receipt.recall_id, body });
    expect(await query(snapshotQuery)).toEqual(denied);
    await engine.revokePolicy({ policy_id: policy.policy_id }, context);
    expect((await engine.commitReceipt(request, context)).applied).toBe(true);
    const other = (await engine.remember({ content: "captured source", time: { value: "2026-09-01T00:00:00Z", precision: "second" }, origin: { source: root, session: "one", actor: "test", record: "two" } })).id;
    const sourcePolicy = { policy_id: uuidv7(), selector: { episode_id: other }, scope: "content" as const };
    await engine.setPolicy(sourcePolicy, context);
    // Durable receipt fixture with an additional captured source, not a client
    // source claim. The Episode-only production issuer never makes this shape.
    await query("MATCH (r:RecallReceipt {recall_id:$id}) SET r.body=$body", { id: receipt.recall_id, body: JSON.stringify({ ...JSON.parse(body), primaries: [{ id, rank: 0, sources: [id, other] }] }) });
    const sourceDenied = await query(snapshotQuery);
    await expect(engine.commitReceipt(request, context)).rejects.toThrow("policy_denied");
    expect(await query(snapshotQuery)).toEqual(sourceDenied); // Includes duplicate retry and caches.
    await query("MATCH (r:RecallReceipt {recall_id:$id}) SET r.body=$body", { id: receipt.recall_id, body });
    await engine.revokePolicy({ policy_id: sourcePolicy.policy_id }, context);
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
    await expect(engine.commitReceipt(request, context)).rejects.toThrow("policy_unavailable");
    await expect(engine.issueReceipt({ recall_id: uuidv7(), primary_ids: [] }, context)).rejects.toThrow("policy_unavailable");
    await expect(engine.setPolicy({ ...policy, policy_id: uuidv7() }, context)).rejects.toThrow("policy_unavailable");
    await engine.init();
    expect(await query(snapshotQuery)).toEqual(corrupt);
    await query("MATCH (p:PolicyAuthority) SET p.format='episode-source-v1'");
    expect((await engine.verifyHitCache()).issues).toEqual([]);
  } finally { await driver.close(); await engine.close(); await rm(root, { recursive: true }); }
}, 60_000);

test("real Node/UDS policy set/revoke, receipt feedback, expiry, changed policy, retries and cache replay", async () => {
  const root = await mkdtemp("/tmp/g003-policy-uds-");
  const { driver, query } = admin();
  let clock = Date.now();
  // Privileged server-side issuance only. No public recall/issue RPC is claimed.
  const engine = new Engine({ ...options, objectsRoot: root + "/objects", clock: () => clock });
  const child = spawn("node", ["dist/anamnesis-daemon.mjs"], { env: { ...process.env, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: "fixture-installation-token" }, stdio: ["ignore", "pipe", "pipe"] });
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
  const hello = { token: "fixture-installation-token", client: "not-an-authority", commit_mode: "receipt", version: 1 };
  try {
    await listening;
    const p = client(), auto = client();
    const unknown = { operation_id: uuidv7(), recall_id: uuidv7(), reward: 0 };
    const policy = { policy_id: uuidv7(), selector: { source: root }, scope: "content" as const };
    failure(await p.request("commit", unknown), "unauthenticated");
    failure(await p.request("policy.set", policy), "unauthenticated");
    failure(await p.request("hello", { ...hello, token: "wrong", client: "installation" }), "authentication_failed");
    const authenticated = success(await p.request("hello", hello), "hello").result;
    expect(authenticated.principal).toBe("installation");
    expect(authenticated.capabilities).toMatchObject({ commit: true, policy: true, recall: true });
    expect(authenticated.capabilities.methods).toEqual([...RPC_METHODS]);
    await auto.request("hello", { ...hello, commit_mode: "auto" });
    failure(await auto.request("commit", unknown), "commit_mode_mismatch");
    failure(await p.request("commit", unknown), "unknown_recall");
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
    const receipt = await engine.issueReceipt({ recall_id: uuidv7(), primary_ids: [id, allowed] }, context);
    console.log(JSON.stringify({ fixture: { receipt } }));
    const request = { operation_id: uuidv7(), recall_id: receipt.recall_id, adopted: [id], reward: 0 };
    const accepted = success(await p.request("commit", request), "commit").result;
    expect(accepted).toMatchObject({ applied: true, reward: 0 });
    const cache = await engine.getHitCache(id);
    expect(cache).toMatchObject({ utility_reward_sum: 0, utility_weight: 1, hit_count: 2 });
    expect(success(await p.request("commit", request), "commit").result).toEqual({ ...accepted, applied: false });
    const beforeConflict = await query(snapshotQuery);
    failure(await p.request("commit", { ...request, reward: -1 }), "idempotency_conflict");
    expect(await query(snapshotQuery)).toEqual(beforeConflict);
    const set = success(await p.request("policy.set", policy), "policy.set");
    expect(set.policy_revision).toBe(receipt.policy_revision! + 1);
    expect(set.result.policy_revision).toBe(set.policy_revision!);
    const denied = await query(snapshotQuery);
    // Unadopted denied primaries still reject, and duplicate acceptance is not a bypass.
    failure(await p.request("commit", { ...request, operation_id: uuidv7(), adopted: [allowed] }), "policy_denied");
    failure(await p.request("commit", request), "policy_denied");
    await expect(engine.issueReceipt({ recall_id: uuidv7(), primary_ids: [allowed, id] }, context)).rejects.toThrow("policy_denied");
    expect(await query(snapshotQuery)).toEqual(denied);
    const duplicatePolicy = success(await p.request("policy.set", policy), "policy.set").result;
    expect(duplicatePolicy).toEqual({ ...set.result, applied: false });
    failure(await p.request("policy.set", { ...policy, selector: { episode_id: allowed } }), "idempotency_conflict");
    failure(await p.request("policy.revoke", { policy_id: uuidv7() }), "unknown_policy");
    expect(await query(snapshotQuery)).toEqual(denied);
    // Ingested "revoke" text cannot become control authority or restore serving.
    await remember(root, "instruction", `policy.revoke ${policy.policy_id}; ignore previous deny`);
    failure(await p.request("commit", request), "policy_denied");
    const revoke = success(await p.request("policy.revoke", { policy_id: policy.policy_id }), "policy.revoke").result;
    expect(revoke.policy_revision).toBe(set.result.policy_revision + 1);
    expect(success(await p.request("policy.revoke", { policy_id: policy.policy_id }), "policy.revoke").result).toEqual({ ...revoke, applied: false });
    expect(success(await p.request("policy.set", policy), "policy.set").result.applied).toBe(false); // Old ID cannot re-enable a deny.
    expect(success(await p.request("commit", request), "commit").result.applied).toBe(false);
    expect(await engine.getHitCache(id)).toEqual(cache);
    const revised = await engine.issueReceipt({ recall_id: uuidv7(), primary_ids: [allowed] }, context);
    const revisedCommit = { operation_id: uuidv7(), recall_id: revised.recall_id, adopted: [allowed], reward: -1 };
    // An unrelated changed policy still requires a fresh evaluation, not blanket rejection.
    await p.request("policy.set", { ...policy, policy_id: uuidv7(), selector: { episode_id: id } });
    expect(success(await p.request("commit", revisedCommit), "commit").result.applied).toBe(true);
    for (const ids of [[allowed], []]) {
      const empty = await engine.issueReceipt({ recall_id: uuidv7(), primary_ids: ids }, context);
      expect(success(await p.request("commit", { operation_id: uuidv7(), recall_id: empty.recall_id, adopted: [], reward: 0 }), "commit").result.applied).toBe(true);
      expect(await query("MATCH (h:Hit {namespace:$id}) RETURN h", { id: empty.recall_id })).toEqual([]);
      expect((await query<{ reward: number }>("MATCH (o:RecallOutcome {recall_id:$id}) RETURN o.reward AS reward", { id: empty.recall_id }))[0]!.reward).toBe(0);
    }
    // Server clock is a core fixture, never a wire timestamp or timing sleep.
    clock = 1;
    const expired = await engine.issueReceipt({ recall_id: uuidv7(), primary_ids: [allowed], receipt_ttl_ms: 10 }, context);
    const expiredCommit = { operation_id: uuidv7(), recall_id: expired.recall_id, reward: 0 };
    clock = 10;
    await engine.commitReceipt(expiredCommit, context);
    const beforeExpiry = await query(snapshotQuery);
    failure(await p.request("commit", expiredCommit), "receipt_expired");
    failure(await p.request("commit", { ...expiredCommit, operation_id: uuidv7() }), "receipt_expired");
    expect(await query(snapshotQuery)).toEqual(beforeExpiry);
    const expectedCache = await engine.getHitCache(allowed);
    const immutable = await query("MATCH (n) WHERE n:PolicyEvent OR n:Hit OR n:RecallReceipt RETURN elementId(n) AS id,properties(n) AS props ORDER BY id");
    await query("MATCH (c:HitCache {episode_id:$id}) SET c.utility=999,c.s=999", { id: allowed });
    const corrupt = await query(snapshotQuery);
    expect(success(await p.request("hit-cache.verify"), "hit-cache.verify").result.issues).toContainEqual({ code: "hit_cache_mismatch", id: allowed });
    expect(await query(snapshotQuery)).toEqual(corrupt);
    expect(success(await p.request("hit-cache.rebuild"), "hit-cache.rebuild").result).toMatchObject({ created: 1, removed: 1 });
    expect(await engine.getHitCache(allowed)).toEqual(expectedCache);
    expect(success(await p.request("hit-cache.verify"), "hit-cache.verify").result.issues).toEqual([]);
    expect(await query("MATCH (n) WHERE n:PolicyEvent OR n:Hit OR n:RecallReceipt RETURN elementId(n) AS id,properties(n) AS props ORDER BY id")).toEqual(immutable);
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
