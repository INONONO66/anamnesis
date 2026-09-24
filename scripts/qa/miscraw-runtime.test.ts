// Run through the ownership-safe foundation runner. Node, not Bun, ingests.
import { expect, test } from "bun:test";
import { createHash, randomUUID } from "node:crypto";
import { EventEmitter, once } from "node:events";
import { appendFileSync } from "node:fs";
import { mkdir, mkdtemp, readFile, rm, stat, utimes, writeFile } from "node:fs/promises";
import { connect, createServer, type Socket } from "node:net";
import { join, resolve } from "node:path";
import neo4j from "neo4j-driver";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { RpcRememberParams } from "../../packages/protocol/src/rpc.ts";
import { startProcess } from "./runtime-scenarios.ts";

const hash = (bytes: string | Uint8Array) => createHash("sha256").update(bytes).digest("hex");
const canonical = (v: unknown): string => v === null || typeof v !== "object" ? JSON.stringify(v) : Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : `{${Object.entries(v).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0).map(([k, x]) => `${JSON.stringify(k)}:${canonical(x)}`).join(",")}}`;
const revisionKey = (p: RpcRememberParams) => { const o = p.episode.origin; return hash(JSON.stringify([hash(JSON.stringify([o.source, o.session, o.actor, o.record])), p.source_revision])); };
const identity = (p: RpcRememberParams, incarnation: string) => ({ revision_key: revisionKey(p), body_digest: hash(canonical({ digest_version: 1, params: p })), data_incarnation: incarnation });
const json = async (path: string) => JSON.parse(await readFile(path, "utf8"));
const line = (value: object) => JSON.stringify(value) + "\n";

// Exact framed transport gate: optionally park an actual remember request or
// committed response. No fabricated receipt and no timer/poll-based interleave.
async function gate(root: string, hold: "request" | "reply" | false) {
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
      sockets.add(socket); socket.on("error", () => other.destroy());
      socket.on("close", () => { sockets.delete(socket); other.destroy(); });
    }
    frames(down, (bytes, value) => {
      if (value.method !== "hello") requests.push(value);
      if (hold === "request" && value.method === "remember" && !held) { held = { down, up, bytes }; events.emit("held", value); }
      else up.write(bytes);
    });
    frames(up, (bytes, value) => {
      if (value.method !== "hello") responses.push(value);
      if (hold === "reply" && value.method === "remember" && !held) { held = { down, up, bytes }; events.emit("held", value); }
      else down.write(bytes);
    });
  });
  const path = join(root, `gate-${randomUUID().slice(0, 8)}.sock`);
  const ready = once(server, "listening", { signal: AbortSignal.timeout(5000) }); server.listen(path); await ready;
  return { path, events, requests, responses,
    drop() { if (!held) throw new Error("no held frame"); held.down.destroy(); held.up.destroy(); },
    release() { if (!held || hold !== "reply") throw new Error("no held reply"); held.down.write(held.bytes); hold = false; },
    async close() { for (const socket of sockets) socket.destroy(); await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve())); },
  };
}

test("built Node misc snapshots: exact UDS identities/payload/outbox, UNKNOWN, reply loss, restart, mutation", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("ownership-safe runner must inject Neo4j credentials");
  const evidenceBase = resolve(".omo/evidence/miscraw-runtime");
  await mkdir(evidenceBase, { recursive: true }); const evidence = await mkdtemp(join(evidenceBase, "uds-"));
  const root = await mkdtemp("/tmp/ana-misc-"), token = randomUUID();
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
  const source = join(root, "export"), cp = join(root, "cursor.json"), build = ".omo/evidence/miscraw-runtime/build";
  const run = (checkpoint = cp, socket?: string) => launch([build + "/ops.mjs", "ingest-misc-raw", source + "/", checkpoint], socket);
  const aside = "aside/home/.aside/u/0/sessions/2026-06-30_native/messages.jsonl", index = "aside/home/.aside/u/0/sessions.jsonl";
  const ag = "gemini-antigravity/home/.gemini/antigravity-cli/brain/brain/.system_generated/logs/transcript.jsonl", oc = "opencode/home/.local/state/opencode/prompt-history.jsonl";
  let failure: unknown;
  try {
    const hashes: Record<string, string> = {};
    for (const path of ["app/anamnesis/miscraw.ts", "app/anamnesis/source.ts", "app/anamnesis/ops.ts", "packages/backfill/src/miscraw.ts", "scripts/qa/miscraw-runtime.test.ts", build + "/main.mjs", build + "/ops.mjs"]) hashes[path] = hash(await readFile(path));
    await record("source-bundles.json", hashes);
    for (const name of ["main", "ops"]) expect(await readFile(build + "/" + name + ".mjs", "utf8")).not.toContain("bun:sqlite");
    let daemon = launch([build + "/main.mjs"], undefined, true); expect(await daemon.ready).toBe(true);
    client = await RpcClient.connect(join(root, "anamnesis.sock"), token);
    const status = await client.request("status", {}); expect(status.storage).toBe("available");
    const secret = "xoxb-1234567890abcdef", raw = "x".repeat(600_000) + " " + secret, masked = raw.replace(secret, "[REDACTED]"), payload = Buffer.from(masked);
    const asideTime = "2026-06-30T04:43:10.501Z", agTime = "2026-06-14T07:35:41.000Z", ocTime = "2026-07-14T03:33:20.000Z";
    const step = (content: string) => ({ type: "USER_INPUT", step_index: 42, created_at: agTime, content: `<USER_REQUEST>\n${content}\n</USER_REQUEST>\n<ADDITIONAL_METADATA>not conversation</ADDITIONAL_METADATA>` });
    const bodies: Record<string, string> = {
      [aside]: line({ role: "system-message", content: "harness" }) + line({ role: "assistant", timestamp: Date.parse(asideTime), content: [{ type: "thinking", text: "hidden" }, { type: "text", text: raw }, { type: "toolCall", text: "hidden" }] }),
      [index]: line({ id: "native", title: "producer title", cwd: "/workspace" }),
      [ag]: [step("A"), step("A"), { type: "RUN_COMMAND", content: "tool" }, step("B"), { type: "EPHEMERAL_MESSAGE", content: "harness" }, step("A")].map(line).join(""),
      [oc]: line({ input: "paste", parts: [{ type: "text", text: "pasted body" }, { type: "file", text: "hidden" }], mode: "normal" }),
    };
    for (const [name, bytes] of Object.entries(bodies)) { await mkdir(join(source, name.slice(0, name.lastIndexOf("/"))), { recursive: true }); await writeFile(join(source, name), bytes); }
    await utimes(join(source, oc), new Date(ocTime), new Date(ocTime));
    await writeFile(join(source, "miscraw.snapshot.json"), line({ format: "misc-raw-snapshot/1", sealed: true, files: Object.entries(bodies).map(([path, bytes]) => ({ path, sha256: hash(bytes), ...(path === oc ? { mtime_ms: Date.parse(ocTime) } : {}) })) }));
    const paramsFor = (source: string, session: string, actor: string, record: string, time: string, text: string, properties: Record<string, string>, previous: string | null = null, revision = hash(time + "\n" + text), long = false) => RpcRememberParams.parse({ episode: {
      schema: "anamnesis.original-message/1", time: { value: time, precision: "second" }, content: long ? masked.slice(0, 4000) : text,
      origin: { source, session, actor, record }, properties,
    }, source_revision: revision, expected_previous_revision_key: previous, ...(long ? { payload_hash: hash(payload) } : {}) });
    const pa = paramsFor("aside", "native", "assistant", "native:1", asideTime, raw, { kind: "message", session_title: "producer title", cwd: "/workspace" }, null, undefined, true);
    const pb = paramsFor("gemini-antigravity", "brain", "user", "brain:42", agTime, "A", { kind: "USER_INPUT" });
    const pc = paramsFor("gemini-antigravity", "brain", "user", "brain:42", agTime, "B", { kind: "USER_INPUT" }, revisionKey(pb));
    const pd = paramsFor("gemini-antigravity", "brain", "user", "brain:42", agTime, "A", { kind: "USER_INPUT" }, revisionKey(pc), pb.source_revision + ":occurrence:5");
    const pe = paramsFor("opencode", "prompt-history", "user", "prompt-history:0", ocTime, "paste\npasted body", { kind: "prompt", mode: "normal" });
    const params = [pa, pb, pb, pc, pd, pe], unique = [pa, pb, pc, pd, pe];
    await record("fixture.json", { files: Object.fromEntries(Object.entries(bodies).map(([name, bytes]) => [name, hash(bytes)])), params, payload: { hash: hash(payload), bytes: payload.length } });

    // Uploaded object exists, but remember never reached daemon. The daemon
    // disowns that identity (UNKNOWN for its own incarnation, storage available),
    // so the resume retires the pending record and resends it from the
    // checkpoint; the already committed object is reused, never re-chunked.
    const unknown = await gate(root, "request"); gates.push(unknown);
    const unknownCp = join(root, "unknown.json"), unknownHeld = once(unknown.events, "held", { signal: AbortSignal.timeout(30_000) });
    const unknownCli = run(unknownCp, unknown.path);
    const unknownOutcome = await Promise.race([unknownHeld.then(([request]) => ({ request })), unknownCli.done.then(result => ({ result }))]);
    if (!("request" in unknownOutcome)) throw new Error(`ops ended before request: ${JSON.stringify(unknownOutcome)}`);
    expect(unknownOutcome.request.params).toEqual(pa); unknown.drop(); const unknownExit = await unknownCli.done; expect(unknownExit.code).toBe(1); expect(unknownExit.output).toContain("outcome_unknown");
    const unknownCursor = await json(unknownCp), unknownPending = await json(unknownCp + ".pending.json");
    expect(unknownCursor.next).toBe(0); expect(unknownPending.identity).toEqual(identity(pa, status.data_incarnation));
    expect(await client.request("ingest.status", unknownPending.identity)).toEqual({ state: "unknown", ...unknownPending.identity });
    expect((await driver.executeQuery("MATCH (e:Episode) RETURN count(e) AS n")).records[0]!.get("n")).toBe(0);
    const unknownObserve = await gate(root, false); gates.push(unknownObserve);
    const disowned = await run(unknownCp, unknownObserve.path).done;
    expect(disowned.code).toBe(0); expect(disowned.output).toContain('"event":"pending_retired","reason":"daemon_unknown","index":0');
    expect(unknownObserve.requests.map(r => r.method)).toEqual(["status", "ingest.status", "object.begin", ...params.map(() => "remember")]);
    expect(unknownObserve.requests.filter(r => r.method === "remember").map(r => r.params)).toEqual(params);
    expect(unknownObserve.responses.find(r => r.method === "object.begin")?.result).toMatchObject({ state: "committed", object: { hash: hash(payload), size: payload.length } });
    const disownedCursor = await json(unknownCp); expect(disownedCursor.next).toBe(params.length); expect(disownedCursor.last).toEqual(identity(pe, status.data_incarnation));
    await expect(stat(unknownCp + ".pending.json")).rejects.toMatchObject({ code: "ENOENT" });
    expect((await driver.executeQuery("MATCH (e:Episode) RETURN count(e) AS n")).records[0]!.get("n")).toBe(unique.length);
    await record("unknown.json", { exit: unknownExit, resumed: disowned, cursor: disownedCursor, pendingIdentity: unknownPending.identity, requests: unknownObserve.requests });

    const loss = await gate(root, "reply"); gates.push(loss);
    const held = once(loss.events, "held", { signal: AbortSignal.timeout(30_000) }), losing = run(cp, loss.path);
    const outcome = await Promise.race([held.then(([reply]) => ({ reply })), losing.done.then(result => ({ result }))]);
    if (!("reply" in outcome)) throw new Error(`ops ended before commit: ${JSON.stringify(outcome)}`);
    // pa was already committed by the disowned resend above: this genuine reply
    // is the daemon's identity match for the same delivery, not a second episode.
    expect(outcome.reply.result).toMatchObject({ ...identity(pa, status.data_incarnation), state: "committed", created: false });
    expect((await json(cp)).next).toBe(0); const pending = await json(cp + ".pending.json");
    expect(pending.params).toEqual(pa); expect(pending.identity).toEqual(identity(pa, status.data_incarnation));
    expect(pending.context).toEqual({ file: aside, line: 2, native_source_revision: pa.source_revision });
    expect(Buffer.from(pending.payload.bytes_b64, "base64")).toEqual(payload);
    loss.drop(); const lost = await losing.done;
    expect(lost.code).toBe(1); expect(lost.output).toContain('"code":"outcome_unknown"'); expect((await json(cp)).next).toBe(0);
    await record("lost-committed-response.json", { reply: outcome.reply, pending: { ...pending, payload: { hash: hash(payload), bytes: payload.length } }, exit: lost });

    await client.close(); client = undefined; daemon.stop(); await daemon.done;
    const oldPid = daemon.pid; daemon = launch([build + "/main.mjs"], undefined, true); expect(await daemon.ready).toBe(true); expect(daemon.pid).not.toBe(oldPid);
    client = await RpcClient.connect(join(root, "anamnesis.sock"), token); expect((await client.request("status", {})).data_incarnation).toBe(status.data_incarnation);
    const observe = await gate(root, false); gates.push(observe);
    const resumed = await run(cp, observe.path).done; expect(resumed.code).toBe(0); expect(resumed.output).toContain('"event":"source_reconciled"');
    expect(observe.requests.filter(r => r.method === "ingest.status").map(r => r.params)).toEqual([identity(pa, status.data_incarnation)]);
    expect(observe.requests.filter(r => r.method === "remember").map(r => r.params)).toEqual(params.slice(1));
    expect(observe.requests.some(r => r.method.startsWith("object."))).toBe(false);
    const replies = [outcome.reply.result, ...observe.responses.filter(r => r.method === "remember").map(r => r.result)];
    for (let i = 0; i < params.length; i++) expect(replies[i]).toMatchObject({ ...identity(params[i]!, status.data_incarnation), state: "committed" });
    expect(replies[2]).toMatchObject({ id: replies[1].id, ingest_seq: replies[1].ingest_seq, created: false });
    expect(unknown.requests.filter(r => r.method === "object.chunk").map(r => r.params.seq)).toEqual([0, 1]);
    expect(await readFile(join(root, "objects", hash(payload).slice(0, 2), hash(payload)))).toEqual(payload);
    const cursor = await json(cp); expect(cursor.next).toBe(6); expect(cursor.last).toEqual(identity(pe, status.data_incarnation));
    expect(cursor.source_hash).toBe(pending.source_hash); expect(cursor.data_incarnation).toBe(status.data_incarnation);
    await expect(stat(cp + ".pending.json")).rejects.toMatchObject({ code: "ENOENT" });
    const repeatObserve = await gate(root, false); gates.push(repeatObserve);
    const beforeRepeat = await readFile(cp); expect((await run(cp, repeatObserve.path).done).code).toBe(0); expect(await readFile(cp)).toEqual(beforeRepeat);
    expect(repeatObserve.requests.map(r => r.method)).toEqual(["status", "ingest.status"]);

    const rows = (await driver.executeQuery("MATCH (e:Episode) RETURN properties(e) AS e ORDER BY e.ingest_seq")).records.map(r => r.get("e"));
    expect(rows).toHaveLength(5); expect(rows.map(e => e.id)).toEqual([replies[0].id, replies[1].id, replies[3].id, replies[4].id, replies[5].id]);
    expect(new Set(rows.map(e => e.id)).size).toBe(5); expect(new Set(rows.map(e => e.ingest_seq)).size).toBe(5);
    for (let i = 0; i < unique.length; i++) {
      const p = unique[i]!, row = rows[i], o = p.episode.origin;
      expect([row.origin_source, row.origin_session, row.origin_actor, row.origin_record]).toEqual([o.source, o.session, o.actor, o.record]);
      expect(row.revision_key).toBe(revisionKey(p)); expect(row.source_revision).toBe(p.source_revision); expect(row.time_value).toBe(p.episode.time.value);
      expect(row.previous_revision_key ?? null).toBe(p.expected_previous_revision_key); expect(row.content).toBe(p.episode.content); expect(row.payload_hash ?? undefined).toBe(p.payload_hash);
      const properties = JSON.parse(row.properties); delete properties.payload_hash; expect(properties).toEqual(p.episode.properties);
      expect(await client.request("ingest.status", identity(p, status.data_incarnation))).toMatchObject({ state: "committed", id: row.id, ingest_seq: row.ingest_seq });
    }
    const effects = (await driver.executeQuery("MATCH (e:Episode) OPTIONAL MATCH (o:Outbox {element_id:e.id}) WITH e, count(o) AS effects RETURN e.id AS id, effects ORDER BY e.ingest_seq")).records.map(r => r.toObject());
    expect(effects).toEqual(rows.map(e => ({ id: e.id, effects: 1 }))); expect((await client.request("status", {})).outbox_pending).toBe(5);
    const topology = (await driver.executeQuery("MATCH (a:Episode)-[:NEXT_EPISODE]->(b:Episode) RETURN a.revision_key AS from, b.revision_key AS to ORDER BY a.ingest_seq")).records.map(r => r.toObject());
    expect(topology).toEqual([{ from: revisionKey(pb), to: revisionKey(pc) }, { from: revisionKey(pc), to: revisionKey(pd) }]);
    await record("acceptance.json", { cursor, params, replies, rows, effects, topology, restart: { oldPid, newPid: daemon.pid, incarnation: status.data_incarnation }, payload: { hash: hash(payload), bytes: payload.length }, requests: observe.requests });

    // Alter joined metadata only after a genuine duplicate COMMIT has been
    // observed. Post-reply consistency check must preserve cursor and pending.
    const mutation = await gate(root, "reply"); gates.push(mutation);
    const mutationCp = join(root, "mutation.json"), mutationHeld = once(mutation.events, "held", { signal: AbortSignal.timeout(30_000) });
    const mutating = run(mutationCp, mutation.path);
    const mutationOutcome = await Promise.race([mutationHeld.then(([reply]) => ({ reply })), mutating.done.then(result => ({ result }))]);
    if (!("reply" in mutationOutcome)) throw new Error(`ops ended before mutation: ${JSON.stringify(mutationOutcome)}`);
    expect(mutationOutcome.reply.result.state).toBe("committed");
    await writeFile(join(source, index), line({ id: "native", title: "changed", cwd: "/workspace" })); mutation.release();
    const rejected = await mutating.done; expect(rejected.code).toBe(1); expect(rejected.output).toContain("source_changed"); expect((await json(mutationCp)).next).toBe(0);
    const unchangedCp = await readFile(mutationCp), unchangedPending = await readFile(mutationCp + ".pending.json");
    const mutationObserve = await gate(root, false); gates.push(mutationObserve);
    const mutationResume = await run(mutationCp, mutationObserve.path).done;
    expect(mutationResume.code).toBe(1); expect(mutationResume.output).toContain("source_seal_mismatch"); expect(mutationObserve.requests).toEqual([]);
    expect(await readFile(mutationCp)).toEqual(unchangedCp); expect(await readFile(mutationCp + ".pending.json")).toEqual(unchangedPending);
    expect((await driver.executeQuery("MATCH (e:Episode) RETURN count(e) AS n")).records[0]!.get("n")).toBe(5);
    await record("mutation.json", { rejected, resume: mutationResume, cursor: await json(mutationCp), pendingUnchanged: true });
    await record("result.json", { ok: true, endpoint: uri, evidence, sqlite: "unsupported", normalized: "Aside sessions index only", live_tail: "unsupported", rotation: "unsupported" });
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
  console.log(JSON.stringify({ event: "miscraw_runtime_acceptance", evidence }));
}, 180_000);
