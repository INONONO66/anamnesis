import { spawn, type ChildProcess } from "node:child_process";
import { createHash } from "node:crypto";
import { chmod, mkdir, readFile, rm } from "node:fs/promises";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import { join } from "node:path";
import { acquireInstallation, atomicJson, runtimeRoot } from "./config.ts";

const execFileAsync = promisify(execFile);
export type ManagedIntent = "start" | "status" | "stop" | "restart";
export interface ManagedTarget { pid: number; identity: string; }
export interface ManagedState extends ManagedTarget { version: 1; intent: ManagedIntent; sequence: number; updated_at: string; }

export function managedStatePath(root: string): string { return join(root, "managed.state.json"); }
export function managedLockPath(root: string): string { return join(root, "managed.lock"); }

/** A repeatable fingerprint of the process identity observed by the OS.
 * PID alone is deliberately insufficient: a reused PID has different command metadata. */
export async function processIdentity(pid: number): Promise<string> {
  if (!Number.isSafeInteger(pid) || pid < 1) throw new Error("invalid managed pid");
  let command: string;
  try { ({ stdout: command } = await execFileAsync("ps", ["-p", String(pid), "-o", "command="])); }
  catch { throw new Error("managed process identity unavailable"); }
  if (!command.trim()) throw new Error("managed process identity unavailable");
  return createHash("sha256").update(`${pid}\\0${command.trim()}`).digest("hex");
}

async function withManagedLock<T>(root: string, action: () => Promise<T>): Promise<T> {
  await mkdir(root, { recursive: true, mode: 0o700 }); await chmod(root, 0o700);
  const lock = managedLockPath(root);
  try { await mkdir(lock, { mode: 0o700 }); }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "EEXIST") throw new Error("managed operation already in progress"); throw error; }
  try { return await action(); } finally { await rm(lock, { recursive: true, force: true }); }
}

async function currentState(root: string): Promise<ManagedState | undefined> {
  try { return JSON.parse(await readFile(managedStatePath(root), "utf8")) as ManagedState; }
  catch (error) { if ((error as NodeJS.ErrnoException).code === "ENOENT") return undefined; throw error; }
}

/** Admission-only lifecycle journal. It records intent and target identity; a future
 * service orchestrator owns the actual start/stop handoff and health proofs. */
export async function managedOperation(root: string, intent: ManagedIntent, target: ManagedTarget): Promise<ManagedState> {
  return withManagedLock(root, async () => {
    const observed = await processIdentity(target.pid);
    if (observed !== target.identity) throw new Error("managed process identity mismatch");
    const previous = await currentState(root);
    if (previous && previous.pid !== target.pid) throw new Error("managed state targets a different process");
    if (previous && previous.identity !== target.identity) throw new Error("managed state identity mismatch");
    const state: ManagedState = { version: 1, intent, pid: target.pid, identity: target.identity,
      sequence: (previous?.sequence ?? 0) + 1, updated_at: new Date().toISOString() };
    await atomicJson(managedStatePath(root), state);
    return state;
  });
}

export async function managedStatus(root: string): Promise<ManagedState | { state: "unknown"; reason: "not_found" | "stale_identity" }> {
  const state = await currentState(root);
  if (!state) return { state: "unknown", reason: "not_found" };
  try { if (await processIdentity(state.pid) !== state.identity) return { state: "unknown", reason: "stale_identity" }; }
  catch { return { state: "unknown", reason: "stale_identity" }; }
  return state;
}

/** Foreground supervisor: owns only its spawned child, never a discovered PID.
 * Restart budget is finite, with no readiness polling or endless crash loop. */
export async function managed(entry: string): Promise<void> {
  const lease = await acquireInstallation(join(runtimeRoot(), "manager"));
  let child: ChildProcess | undefined;
  let stopping = false;
  let deadline: ReturnType<typeof setTimeout> | undefined;
  const stop = () => {
    stopping = true;
    child?.kill("SIGTERM");
    deadline ??= setTimeout(() => { child?.kill("SIGKILL"); process.exitCode = 1; }, 120_000);
  };
  process.on("SIGINT", stop);
  process.on("SIGTERM", stop);
  try {
    for (let restarts = 0; !stopping; restarts++) {
      await lease.assertOwned();
      const result = await new Promise<{ code: number | null; signal: NodeJS.Signals | null }>((resolve, reject) => {
        child = spawn(process.execPath, [entry, "foreground"], { env: process.env, stdio: ["ignore", "inherit", "inherit"] });
        child.once("error", reject);
        child.once("close", (code, signal) => resolve({ code, signal }));
        console.log(JSON.stringify({ event: "managed_child", pid: child.pid, restarts }));
      });
      child = undefined;
      if (stopping || result.code === 0) break;
      if (restarts === 3) throw new Error("managed_restart_budget_exhausted");
      console.error(JSON.stringify({ event: "managed_restart", ...result }));
    }
  } finally {
    if (deadline) clearTimeout(deadline);
    process.off("SIGINT", stop);
    process.off("SIGTERM", stop);
    await lease.release();
  }
}
