import { AsyncLocalStorage } from "node:async_hooks";
import { createHash } from "node:crypto";
import { appendFileSync, renameSync, rmSync, statSync } from "node:fs";
import type { RpcMethod } from "../../packages/protocol/src/rpc.ts";

export interface TimingEvent {
  layer: "driver" | "client" | "daemon" | "runtime" | "neo4j";
  event: string;
  method?: RpcMethod;
  id?: number | string;
  callSequence?: number;
  trafficSequence?: number | undefined;
  connection?: number;
  operation?: string | undefined;
  elapsedMs?: number;
  deadlineMs?: number;
  bytes?: number;
  hash?: string | undefined;
}
export type TimingSink = (event: TimingEvent) => void;
export const timingHash = (text: string) => createHash("sha256").update(text).digest("hex");

/** Two bounded JSONL segments retain the most recent events, without sampling.
 * Sequence gaps expose rotation. monotonicMs is process-local; at correlates
 * processes but is not a duration clock. Never pass bodies, errors or log text. */
export function timingLog(path: string, maxBytes = 8 * 1024 * 1024): TimingSink {
  let size: number;
  try { size = statSync(path).size; }
  catch (error) { if ((error as NodeJS.ErrnoException).code !== "ENOENT") throw error; size = 0; }
  let eventSequence = 0;
  return value => {
    const { layer, event, method, id, callSequence, trafficSequence, connection, operation, elapsedMs, deadlineMs, bytes, hash } = value;
    const line = JSON.stringify({ at: new Date().toISOString(), monotonicMs: performance.now(), pid: process.pid,
      eventSequence: ++eventSequence, layer, event, method, id: typeof id === "string" ? timingHash(id) : id,
      callSequence, trafficSequence, connection, operation, elapsedMs, deadlineMs, bytes, hash }) + "\n";
    const length = Buffer.byteLength(line);
    if (length > maxBytes) throw new Error("timing event exceeds segment budget");
    if (size + length > maxBytes) { rmSync(path + ".1", { force: true }); renameSync(path, path + ".1"); size = 0; }
    appendFileSync(path, line, { mode: 0o600 }); size += length;
  };
}

// Enabled only by the continuous runner for its owned Node daemon.
export const daemonTiming = process.env["ANAMNESIS_G006_TIMING_PATH"] ? timingLog(process.env["ANAMNESIS_G006_TIMING_PATH"]!) : undefined;
export const timingContext = new AsyncLocalStorage<{ method: RpcMethod; id: number | string; connection: number }>();
export async function runtimeTimed<T>(operation: string, action: () => PromiseLike<T>, hash?: string): Promise<T> {
  if (!daemonTiming) return action();
  const started = performance.now();
  const fields = { layer: "runtime" as const, ...timingContext.getStore(), operation, hash };
  daemonTiming({ ...fields, event: "start" });
  try { const result = await action(); daemonTiming({ ...fields, event: "complete", elapsedMs: performance.now() - started }); return result; }
  catch (error) { daemonTiming({ ...fields, event: "failed", elapsedMs: performance.now() - started, hash: timingHash(String(error)) }); throw error; }
}
