import { createHash, randomUUID } from "node:crypto";
import { appendFileSync, writeFileSync } from "node:fs";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join, resolve } from "node:path";
import { RpcClient } from "../../app/anamnesis/client.ts";
import { socketPath } from "../../app/anamnesis/config.ts";
import { timingHash, timingLog } from "../../app/anamnesis/timing.ts";
import type { RpcMethod, RpcStatusResult } from "../../packages/protocol/src/rpc.ts";
import type { ContinuousSoakDriver } from "./g006-runtime-scenarios.ts";
import type { ProcessResult, startProcess } from "./runtime-scenarios.ts";

export interface OwnedEndpoint { uri: string; user: string; password: string }

export function createContinuousDriver(start: typeof startProcess, evidence: string, workspace: string, signal: AbortSignal, endpoint?: OwnedEndpoint): ContinuousSoakDriver {
  let root: string | undefined;
  let token: string | undefined;
  let daemon: ReturnType<typeof start> | undefined;
  let daemonResult: ProcessResult | undefined;
  let terminalEvidence: Promise<string | undefined> | undefined;
  let client: RpcClient | undefined;
  let identity: { data_incarnation: string; fs_epoch: string } | undefined;
  const receipts = new Map<number, { revision_key: string; body_digest: string; data_incarnation: string }>();
  const redact = (text: string) => [endpoint?.password, token].reduce<string>((text, secret) => secret ? text.replaceAll(secret, "[REDACTED]") : text, text);
  const timing = timingLog(join(evidence, "continuous-timing.jsonl"));
  let eventSequence = 0, callSequence = 0;
  let trafficSequence: number | undefined;
  const event = (value: object) => appendFileSync(join(evidence, "continuous-events.jsonl"), redact(JSON.stringify({ at: new Date().toISOString(), monotonicMs: performance.now(), eventSequence: ++eventSequence, pid: daemon?.pid, ...value })) + "\n");
  const record = (file: string, value: unknown) => writeFileSync(join(evidence, file), redact(JSON.stringify(value, null, 2)) + "\n");
  const observed = async <T>(method: RpcMethod, action: () => Promise<T>): Promise<T> => {
    const started = performance.now();
    const fields = { layer: "driver" as const, method, callSequence: ++callSequence, trafficSequence };
    timing({ ...fields, event: "request" });
    try { const result = await action(); timing({ ...fields, event: "response", elapsedMs: performance.now() - started }); return result; }
    catch (error) {
      timing({ ...fields, event: "failed", elapsedMs: performance.now() - started, hash: timingHash(String(error)) });
      event({ event: "rpc_failed", method, error: String(error), code: (error as { code?: string }).code,
        cause: error instanceof Error ? String(error.cause ?? "") : undefined, stack: error instanceof Error ? error.stack : undefined });
      throw error; // Never reconnect/resend an admitted write or hide daemon death.
    }
  };
  const status = async () => {
    if (!client) throw new Error("continuous driver is not subscribed");
    const value = await observed("status", () => client!.request("status", {}));
    event({ event: "status", status: value });
    checkStatus(value);
    return value;
  };
  const checkStatus = (value: RpcStatusResult) => {
    if (identity && (identity.data_incarnation !== value.data_incarnation || identity.fs_epoch !== value.fs_epoch)) throw new Error("continuous daemon identity changed");
    if (value.storage !== "available" || value.state !== "ready") throw new Error(`continuous storage not ready: ${value.storage}/${value.state}`);
    if (daemonResult) throw new Error(`continuous daemon exited: ${JSON.stringify(daemonResult)}`);
  };

  return {
    async launchOwnedDaemon() {
      if (!endpoint?.password || !endpoint.uri || !endpoint.user) throw new Error("owned runner credentials required (explicit URI, user and password)");
      signal.throwIfAborted();
      root = await mkdtemp("/tmp/anamnesis-g006-");
      token = randomUUID();
      const command = ["node", join(workspace, "dist/anamnesis-daemon.mjs")];
      record("continuous-launch.json", { command, cwd: workspace, root, uri: endpoint.uri, user: endpoint.user, readinessDeadlineMs: 120_000 });
      daemon = start(command[0]!, command.slice(1), {
        cwd: workspace, deadlineMs: 120_000, signal,
        env: { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token,
          ANAMNESIS_G006_TIMING_PATH: resolve(evidence, "continuous-daemon-timing.jsonl"),
          ANAMNESIS_NEO4J_URI: endpoint.uri, ANAMNESIS_NEO4J_USER: endpoint.user, ANAMNESIS_NEO4J_PASSWORD: endpoint.password, ANAMNESIS_NEO4J_DATABASE: "neo4j" },
        readyLine: /"event":"listening"/,
        onOutput: text => {
          timing({ layer: "daemon", event: "output_observed", bytes: Buffer.byteLength(text), hash: timingHash(text) });
          appendFileSync(join(evidence, "continuous-daemon.log"), redact(text));
        },
      });
      // Subscribe before any traffic; retain numeric exit, signal and deadline evidence.
      terminalEvidence = daemon.done.then(result => {
        daemonResult = result; record("continuous-daemon-result.json", result); event({ event: "daemon_exit", ...result });
        return undefined;
      }).catch(error => `daemon terminal evidence failed: ${String(error)}`);
      if (!(await daemon.ready)) throw new Error(`owned daemon did not become ready: ${JSON.stringify(await daemon.done)}`);
      event({ event: "daemon_ready", root });
    },
    async subscribe() {
      if (!root || !token) throw new Error("continuous driver daemon is not launched");
      client = await observed("hello", () => RpcClient.connect(socketPath(root!), token!, "receipt", timing));
      const value = await status();
      const owner = JSON.parse(await readFile(join(root, "owner/owner.json"), "utf8"));
      if (owner.pid !== daemon?.pid || owner.nonce !== value.fs_epoch) throw new Error("continuous daemon owner mismatch");
      identity = { data_incarnation: value.data_incarnation, fs_epoch: value.fs_epoch };
      event({ event: "subscribed", ...identity });
    },
    async traffic(sequence) {
      trafficSequence = sequence;
      await status();
      const params = {
        episode: { schema: "anamnesis.original-message/1" as const, time: { value: "2026-09-09T00:00:00Z", precision: "second" as const },
          content: `G006 continuous traffic ${sequence}`, mass: 1, properties: {}, origin: { source: "g006-soak", session: "continuous", actor: "qa", record: String(sequence) } },
        source_revision: `g006-${sequence}`, expected_previous_revision_key: null,
      };
      // Preserve the exact admitted delivery even if its receipt is lost.
      event({ event: "traffic_admitted", sequence, params, ...identity });
      const receipt = await observed("remember", () => client!.request("remember", params));
      receipts.set(sequence, { revision_key: receipt.revision_key, body_digest: receipt.body_digest, data_incarnation: receipt.data_incarnation });
      event({ event: "traffic_receipt", sequence, receipt });
      if (receipt.state !== "committed") throw new Error(`traffic not committed: ${receipt.state}`);
      if (receipt.data_incarnation !== identity?.data_incarnation) throw new Error("traffic incarnation mismatch");
      // ingest.status reads and validates the actual immutable DB artifact.
      const verified = await observed("ingest.status", () => client!.request("ingest.status", receipts.get(sequence)!));
      event({ event: "traffic_verified", sequence, receipt: verified });
      if (verified.state !== "committed" || verified.id !== receipt.id || verified.ingest_seq !== receipt.ingest_seq) throw new Error(`traffic artifact not verified: ${verified.state}`);
      event({ event: "traffic_committed", sequence, receipt, ...identity });
    },
    async recover(sequence) {
      trafficSequence = sequence;
      await status();
      const delivery = receipts.get(sequence);
      if (!delivery) throw new Error(`missing traffic identity ${sequence}`);
      const settled = await observed("ingest.status", () => client!.request("ingest.status", delivery));
      event({ event: "recovered", sequence, state: settled.state, receipt: settled, ...identity });
      if (settled.state !== "committed") throw new Error(`traffic recovery unresolved: ${settled.state}`);
    },
    async terminal() { trafficSequence = undefined; await status(); signal.throwIfAborted(); event({ event: "terminal_boundary", ...identity }); },
    async hashEvidence() {
      const hashes: Record<string, string> = {};
      for (const path of ["dist/anamnesis-daemon.mjs", "dist/anamnesis-client.mjs", "dist/anamnesis-ops.mjs", "app/anamnesis/recovery.surface.mjs"]) {
        hashes[path] = createHash("sha256").update(await readFile(join(workspace, path))).digest("hex");
      }
      return hashes;
    },
    async cleanup() {
      const failures: string[] = [];
      const ownedRoot = root;
      const timer = setTimeout(() => { failures.push("owned daemon cleanup deadline exceeded"); daemon?.stop(); }, 125_000);
      try {
        try {
          if (client) await observed("shutdown", () => client!.request("shutdown", {}));
          else daemon?.stop();
        } catch (error) { failures.push(String(error)); daemon?.stop(); }
        try { await client?.close(); } catch (error) { failures.push(String(error)); daemon?.stop(); }
        if (daemon) {
          const result = await daemon.done;
          const evidenceError = await terminalEvidence;
          if (evidenceError) failures.push(evidenceError);
          if (result.code !== 0 || result.signal || result.timedOut) failures.push(`owned daemon exit=${result.code}, signal=${result.signal}, timedOut=${result.timedOut}`);
        }
      } finally {
        clearTimeout(timer);
        client = undefined;
        try { if (root) await rm(root, { recursive: true, force: true }); root = undefined; }
        catch (error) { failures.push(String(error)); }
        record("continuous-cleanup.json", { root: ownedRoot ?? null, rootRemoved: root === undefined, daemon: daemonResult ?? null, errors: failures });
        event({ event: "cleanup_complete", root: ownedRoot, rootRemoved: root === undefined, errors: failures, ...identity });
        daemon = undefined;
      }
      if (failures.length) throw new Error(failures.join("; "));
    },
  };
}
