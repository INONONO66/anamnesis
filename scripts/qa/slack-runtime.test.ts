// Bun hosts assertions; only private built Node daemon/ops ingest the fixture.
// Build: bun build app/anamnesis/{main,ops}.ts --target=node --outdir=build/slack-repair
// Run only through runtime-scenarios.ts --case foundation --test-path this file.
import { expect, test } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { EventEmitter, once } from "node:events";
import { appendFileSync } from "node:fs";
import { mkdtemp, mkdir, readFile, rm, stat, writeFile } from "node:fs/promises";
import { connect, createServer, type Socket } from "node:net";
import { join, resolve } from "node:path";
import neo4j from "neo4j-driver";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { RPC_LIMITS, RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { startProcess } from "./runtime-scenarios.ts";

const hash = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const canonical = (v: unknown): string => v === null || typeof v !== "object" ? JSON.stringify(v) : Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : `{${Object.entries(v).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([k, x]) => `${JSON.stringify(k)}:${canonical(x)}`).join(",")}}`;
const revisionKey = (p: RpcRememberParams) => { const o = p.episode.origin; return hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), p.source_revision])); };
const identity = (p: RpcRememberParams, incarnation: string) => ({ revision_key: revisionKey(p), body_digest: hash(canonical({ digest_version: 1, params: p })), data_incarnation: incarnation });
const json = async (path: string) => JSON.parse(await readFile(path, "utf8"));

// Observe exact real frames and optionally withhold one committed reply. Never
// fabricate a receipt; subscribe to held before triggering the ops process.
async function gate(root: string, hold: boolean) {
  const events = new EventEmitter(), sockets = new Set<Socket>();
  const requests: { method: string; params: any }[] = [], responses: { method: string; result?: any; error?: unknown }[] = [];
  let held: { down: Socket; up: Socket; bytes: Buffer } | undefined;
  function frames(socket: Socket, accept: (bytes: Buffer, value: any) => void) {
    let buffer: Buffer = Buffer.alloc(0);
    socket.on("data", (chunk: Buffer) => {
      buffer = Buffer.concat([buffer, chunk]);
      while (buffer.length >= 4 && buffer.length >= 4 + buffer.readUInt32BE()) {
        const end = 4 + buffer.readUInt32BE(), frame = buffer.subarray(0, end);
        buffer = buffer.subarray(end); accept(frame, JSON.parse(frame.subarray(4).toString()));
      }
    });
  }
  const server = createServer(down => {
    const up = connect(join(root, "anamnesis.sock"));
    for (const [socket, other] of [[down, up], [up, down]] as const) {
      sockets.add(socket);
      socket.on("error", () => other.destroy());
      socket.on("close", () => { sockets.delete(socket); other.destroy(); });
    }
    frames(down, (bytes, value) => { if (value.method !== "hello") requests.push(value); up.write(bytes); });
    frames(up, (bytes, value) => {
      if (value.method !== "hello") responses.push(value);
      if (hold && value.method === "remember" && !held) { held = { down, up, bytes }; events.emit("held", value); }
      else down.write(bytes);
    });
  });
  const path = join(root, `gate-${randomUUID().slice(0, 8)}.sock`);
  const ready = once(server, "listening", { signal: AbortSignal.timeout(5000) }); server.listen(path); await ready;
  return { path, events, requests, responses,
    drop() { if (!held) throw new Error("no held reply"); held.down.destroy(); held.up.destroy(); },
    release() { if (!held) throw new Error("no held reply"); held.down.write(held.bytes); hold = false; },
    async close() { for (const socket of sockets) socket.destroy(); await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve())); },
  };
}

test("built Node Slack snapshots: real UDS commit, revision identity, payload, loss/resume and mutation", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("ownership-safe runner must inject Neo4j credentials");
  await mkdir(".omo/evidence/slack-repair", { recursive: true });
  const evidence = await mkdtemp(resolve(".omo/evidence/slack-repair/uds-"));
  const root = await mkdtemp("/tmp/ana-slack-"), token = randomUUID();
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_USER: process.env["ANAMNESIS_TEST_NEO4J_USER"] ?? "neo4j", ANAMNESIS_NEO4J_PASSWORD: password };
  const redact = (text: string) => text.replaceAll(password, "[REDACTED]").replaceAll(token, "[REDACTED]");
  const record = (name: string, value: unknown) => writeFile(join(evidence, name), redact(JSON.stringify(value, null, 2) + "\n"));
  const processes: ReturnType<typeof startProcess>[] = [], gates: Awaited<ReturnType<typeof gate>>[] = [];
  let client: RpcClient | undefined;
  const driver = neo4j.driver(uri, neo4j.auth.basic(env.ANAMNESIS_NEO4J_USER, password), { disableLosslessIntegers: true, connectionTimeout: 10_000, maxTransactionRetryTime: 0 });
  const launch = (args: string[], socket?: string, daemon = false) => {
    appendFileSync(join(evidence, "commands.jsonl"), JSON.stringify(["node", ...args]) + "\n");
    const child = startProcess("node", args, { deadlineMs: daemon ? 30_000 : 60_000, env: { ...env, ...(socket ? { ANAMNESIS_RUNTIME_SOCKET: socket } : {}) }, ...(daemon ? { readyLine: /"event":"listening"/ } : {}), onOutput: text => appendFileSync(join(evidence, "processes.txt"), redact(text)) });
    processes.push(child); return child;
  };
  const source = join(root, "export"), cp = join(root, "cursor.json");
  const run = (checkpoint = cp, socket?: string) => launch(["build/slack-repair/ops.js", "ingest-slack", source + "/", checkpoint], socket);
  let failure: unknown;
  try {
    const hashes: Record<string, string> = {};
    for (const path of ["app/anamnesis/slack.ts", "app/anamnesis/source.ts", "packages/backfill/src/slack.ts", "scripts/qa/slack-runtime.test.ts", "build/slack-repair/main.js", "build/slack-repair/ops.js"]) hashes[path] = hash(await readFile(path));
    await record("source-bundles.json", hashes);
    const daemon = launch(["build/slack-repair/main.js"], undefined, true);
    expect(await daemon.ready).toBe(true);
    client = await RpcClient.connect(join(root, "anamnesis.sock"), token);
    const status = await client.request("status", {});
    expect(status.storage).toBe("available");
    await mkdir(join(source, "channels"), { recursive: true }); await mkdir(join(source, "threads"));
    const index = JSON.stringify({ id: "C1", name: "general" }) + "\n";
    const a = { ts: "21.0", user: "U", text: "A sk-abcdefghijklmnopqrstuvwxyz123456", edited: { ts: "22.0" } };
    const b = { ...a, text: "B", edited: { ts: "23.0" } };
    const payload = Buffer.from("猫".repeat(200_000) + " [REDACTED]");
    const long = { ts: "30.0", user: "U2", text: "猫".repeat(200_000) + " sk-abcdefghijklmnopqrstuvwxyz123456", thread_ts: "21.0" };
    const channel = [{ ts: "1.0", subtype: "channel_join", text: "housekeeping" }, { ts: "2.0", text: "<@U>さんがチャンネルに参加しました" }, a, b, long].map(x => JSON.stringify(x)).join("\n") + "\n";
    const thread = [b, a, a].map(x => JSON.stringify(x)).join("\n") + "\n";
    await writeFile(join(source, "index.jsonl"), index); await writeFile(join(source, "channels/C1.jsonl"), channel); await writeFile(join(source, "threads/C1-21.0.jsonl"), thread);
    const sourceHash = hash(JSON.stringify({ format: "slack-export-snapshot/1", manifest: [{ file: "index.jsonl", sha256: hash(index) }, { file: "channels/C1.jsonl", sha256: hash(channel) }, { file: "threads/C1-21.0.jsonl", sha256: hash(thread) }] }));
    const expected = (revision: string, content: string, previous: string | null, large = false) => RpcRememberParams.parse({ episode: {
      schema: "anamnesis.original-message/1", time: { value: large ? "1970-01-01T00:00:30.000Z" : "1970-01-01T00:00:21.000Z", precision: "second" }, content,
      origin: { source: "slack", session: "C1", actor: large ? "U2" : "U", record: large ? "30.0" : "21.0" },
      properties: { channel_name: "general", slack_ts: large ? "30.0" : "21.0", ...(large ? { thread_parent_ts: "21.0" } : {}) },
    }, source_revision: revision, expected_previous_revision_key: previous, ...(large ? { payload_hash: hash(payload) } : {}) });
    const pa = expected("22.0", "A [REDACTED]", null), pb = expected("23.0", "B", revisionKey(pa));
    const pl = expected("30.0", "猫".repeat(Math.floor(RPC_LIMITS.content_bytes / 3)), null, true);
    const pr = expected("22.0:occurrence:5", "A [REDACTED]", revisionKey(pb));
    const params = [pa, pb, pl, pb, pr, pr], unique = [pa, pb, pl, pr];

    const loss = await gate(root, true); gates.push(loss);
    const held = once(loss.events, "held", { signal: AbortSignal.timeout(30_000) });
    const losing = run(cp, loss.path);
    const outcome = await Promise.race([held.then(([reply]) => ({ reply })), losing.done.then(result => ({ result }))]);
    if (!("reply" in outcome)) throw new Error(`ops ended before commit: ${JSON.stringify(outcome)}`);
    expect(outcome.reply.result.state).toBe("committed");
    expect(outcome.reply.result).toMatchObject(identity(pa, status.data_incarnation));
    expect((await json(cp)).next).toBe(0);
    const pending = await json(cp + ".pending.json");
    expect(pending.params).toEqual(pa); expect(pending.identity).toEqual(identity(pa, status.data_incarnation));
    expect(pending.context).toEqual({ file: "channels/C1.jsonl", line: 3, native_source_revision: "22.0" });
    loss.drop(); const lost = await losing.done;
    expect(lost.code).toBe(1); expect(lost.output).toContain('"code":"outcome_unknown"'); expect((await json(cp)).next).toBe(0);
    await record("lost-committed-response.json", { reply: outcome.reply, pending, exit: lost });

    const observe = await gate(root, false); gates.push(observe);
    const resumed = await run(cp, observe.path).done;
    expect(resumed.code).toBe(0); expect(resumed.output).toContain('"event":"source_reconciled"');
    expect(observe.requests.filter(r => r.method === "ingest.status").map(r => r.params)).toEqual([identity(pa, status.data_incarnation)]);
    expect(observe.requests.filter(r => r.method === "remember").map(r => r.params)).toEqual(params.slice(1));
    const replies = [outcome.reply.result, ...observe.responses.filter(r => r.method === "remember").map(r => r.result)];
    for (let i = 0; i < params.length; i++) { expect(replies[i]).toMatchObject({ ...identity(params[i]!, status.data_incarnation), state: "committed" }); }
    expect(replies[3]).toMatchObject({ id: replies[1].id, ingest_seq: replies[1].ingest_seq, created: false });
    expect(replies[5]).toMatchObject({ id: replies[4].id, ingest_seq: replies[4].ingest_seq, created: false });
    expect(observe.requests.filter(r => r.method === "object.chunk").map(r => r.params.seq)).toEqual([0, 1]);
    expect(await readFile(join(root, "objects", hash(payload).slice(0, 2), hash(payload)))).toEqual(payload);
    const cursor = await json(cp);
    expect(cursor).toEqual({ version: 1, source_hash: sourceHash, data_incarnation: status.data_incarnation, next: 6, last: identity(pr, status.data_incarnation) });
    expect(await Bun.file(cp + ".pending.json").exists()).toBe(false);
    const beforeRepeat = await readFile(cp);
    expect((await run().done).code).toBe(0); expect(await readFile(cp)).toEqual(beforeRepeat);

    const rows = (await driver.executeQuery("MATCH (e:Episode {origin_source:'slack'}) RETURN properties(e) AS e ORDER BY e.ingest_seq")).records.map(r => r.get("e"));
    expect(rows.length).toBe(4);
    expect(rows.map(e => e.id)).toEqual([replies[0].id, replies[1].id, replies[2].id, replies[4].id]);
    expect(new Set(rows.map(e => e.id)).size).toBe(4); expect(new Set(rows.map(e => e.ingest_seq)).size).toBe(4);
    for (let i = 0; i < unique.length; i++) {
      const p = unique[i]!, row = rows[i];
      expect(row.revision_key).toBe(revisionKey(p)); expect(row.source_revision).toBe(p.source_revision);
      expect(row.previous_revision_key ?? null).toBe(p.expected_previous_revision_key);
      expect(row.content).toBe(p.episode.content); expect(row.payload_hash ?? undefined).toBe(p.payload_hash);
      const properties = JSON.parse(row.properties); delete properties.payload_hash;
      expect(properties).toEqual(p.episode.properties);
      expect(await client.request("ingest.status", identity(p, status.data_incarnation))).toMatchObject({ state: "committed", id: row.id, ingest_seq: row.ingest_seq });
    }
    const effects = (await driver.executeQuery("MATCH (e:Episode {origin_source:'slack'}) OPTIONAL MATCH (o:Outbox {element_id:e.id}) WITH e, count(o) AS effects RETURN e.id AS id, effects ORDER BY e.ingest_seq")).records.map(r => r.toObject());
    expect(effects).toEqual(rows.map(e => ({ id: e.id, effects: 1 })));
    expect((await client.request("status", {})).outbox_pending).toBe(4);
    await record("acceptance.json", { cursor, params, replies, rows, effects, payload: { hash: hash(payload), bytes: payload.length }, requests: observe.requests.map(r => ({ method: r.method, ...(r.method === "object.chunk" ? { seq: r.params.seq } : { params: r.params }) })) });

    // Modify the producer-sealed file only after the gate observes real COMMIT,
    // then release the reply. The cursor must not acknowledge the changed source.
    const mutation = await gate(root, true); gates.push(mutation);
    const mutationCp = join(root, "mutation-cursor.json");
    const mutationHeld = once(mutation.events, "held", { signal: AbortSignal.timeout(30_000) });
    const mutating = run(mutationCp, mutation.path);
    const mutationOutcome = await Promise.race([mutationHeld.then(([reply]) => ({ reply })), mutating.done.then(result => ({ result }))]);
    if (!("reply" in mutationOutcome)) throw new Error(`ops ended before mutation: ${JSON.stringify(mutationOutcome)}`);
    expect(mutationOutcome.reply.result.state).toBe("committed");
    await writeFile(join(source, "index.jsonl"), JSON.stringify({ id: "C1", name: "changed" }) + "\n");
    mutation.release(); const rejected = await mutating.done;
    expect(rejected.code).toBe(1); expect(rejected.output).toContain("source_changed");
    expect((await json(mutationCp)).next).toBe(0);
    const unchangedCp = await readFile(mutationCp), unchangedPending = await readFile(mutationCp + ".pending.json");
    const mutationResume = await run(mutationCp).done;
    expect(mutationResume.code).toBe(1); expect(mutationResume.output).toContain("source_changed");
    expect(await readFile(mutationCp)).toEqual(unchangedCp); expect(await readFile(mutationCp + ".pending.json")).toEqual(unchangedPending);
    expect((await driver.executeQuery("MATCH (e:Episode {origin_source:'slack'}) RETURN count(e) AS n")).records[0]!.get("n")).toBe(4);
    await record("mutation.json", { rejected, resume: mutationResume, cursor: await json(mutationCp), pendingUnchanged: true });
    await record("result.json", { ok: true, endpoint: uri, evidence });
  } catch (error) { failure = error; await record("result.json", { ok: false, error: String(error) }); }
  finally {
    const cleanup: { name: string; ok: boolean; error?: string }[] = [];
    const attempt = async (name: string, work: () => Promise<unknown>) => {
      try { await work(); cleanup.push({ name, ok: true }); }
      catch (error) { cleanup.push({ name, ok: false, error: String(error) }); failure = new AggregateError(failure ? [failure, error] : [error], `${name} cleanup failed`); }
    };
    await attempt("client", async () => { await client?.close(); });
    await attempt("processes", async () => { for (const child of processes) { child.stop(); await child.done; if (child.pid) expect(() => process.kill(child.pid!, 0)).toThrow(); } });
    await attempt("gates", async () => { for (const relay of gates) await relay.close(); });
    await attempt("driver", () => driver.close());
    await attempt("root", async () => { await rm(root, { recursive: true, force: true }); await expect(stat(root)).rejects.toMatchObject({ code: "ENOENT" }); });
    await record("cleanup.json", { root, pids: processes.map(p => p.pid), cleanup });
  }
  if (failure) throw failure;
  console.log(JSON.stringify({ event: "slack_runtime_acceptance", evidence }));
}, 180_000);
