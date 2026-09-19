import { lstat, readdir, rename } from "node:fs/promises";
import { dirname, resolve } from "node:path";
import { atomicJson, syncDirectory, hasCode } from "./config.ts";
import { preflightArchive, type ArchiveCompatibility, type AdmittedArchive } from "./archive-manifest.ts";

export type RestorePhase = "STAGED_VERIFIED" | "WILL_RENAME_LIVE" | "LIVE_RENAMED" | "WILL_PROMOTE" | "STAGING_PROMOTED" | "WILL_START" | "STARTED" | "WILL_FENCE_PROMOTED" | "PROMOTED_FENCED" | "WILL_QUARANTINE_PROMOTED" | "PROMOTED_QUARANTINED" | "WILL_ROLLBACK" | "ROLLBACK_PROMOTED" | "ROLLED_BACK";
export interface RestorePaths { live: string; staging: string; rollback: string; failed: string; state: string; }
export interface RestoreState { format: "anamnesis.restore-state/1"; operation_id: string; paths: RestorePaths; phase: RestorePhase; }
export class RestoreAuthorityError extends Error { readonly code: "unsafe_path" | "restore_pending" | "incompatible_archive" | "operation_locked"; constructor(code: "unsafe_path" | "restore_pending" | "incompatible_archive" | "operation_locked", detail: string) { super(`${code}: ${detail}`); this.code = code; } }
const UUID7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const phases = new Set<RestorePhase>(["STAGED_VERIFIED", "WILL_RENAME_LIVE", "LIVE_RENAMED", "WILL_PROMOTE", "STAGING_PROMOTED", "WILL_START", "STARTED", "WILL_FENCE_PROMOTED", "PROMOTED_FENCED", "WILL_QUARANTINE_PROMOTED", "PROMOTED_QUARANTINED", "WILL_ROLLBACK", "ROLLBACK_PROMOTED", "ROLLED_BACK"]);
const exists = async (path: string) => { try { await lstat(path); return true; } catch (e) { if (hasCode(e, "ENOENT")) return false; throw e; } };
async function dir(path: string) { const s = await lstat(path); if (!s.isDirectory() || s.isSymbolicLink()) throw new RestoreAuthorityError("unsafe_path", "root must be a real directory"); return s; }
function safe(path: string) { if (!path.startsWith("/") || resolve(path) !== path || path === "/") throw new RestoreAuthorityError("unsafe_path", "absolute canonical path required"); }
async function empty(path: string) { return (await readdir(path)).length === 0; }
function stateShape(value: unknown): RestoreState { if (!value || typeof value !== "object") throw new RestoreAuthorityError("unsafe_path", "invalid restore journal"); const s = value as RestoreState; if (s.format !== "anamnesis.restore-state/1" || !phases.has(s.phase) || !UUID7.test(s.operation_id)) throw new RestoreAuthorityError("unsafe_path", "invalid restore journal"); return s; }

export async function preflightRestore(archive: string, compatibility: ArchiveCompatibility): Promise<AdmittedArchive> { return preflightArchive(archive, compatibility); }

export async function createRestoreActivation(input: { liveRoot: string; stagingRoot: string; rollbackRoot: string; statePath: string; operationId: string; failedRoot?: string; }): Promise<RestoreActivation> {
  const paths: RestorePaths = { live: input.liveRoot, staging: input.stagingRoot, rollback: input.rollbackRoot, failed: input.failedRoot ?? `${input.liveRoot}.failed.${input.operationId}`, state: input.statePath };
  if (!UUID7.test(input.operationId)) throw new RestoreAuthorityError("unsafe_path", "UUIDv7 operation identity required");
  for (const path of Object.values(paths)) safe(path);
  const parent = await dir(dirname(paths.live));
  for (const path of [paths.live, paths.staging, paths.rollback]) { const stat = await dir(path); if (stat.dev !== parent.dev) throw new RestoreAuthorityError("unsafe_path", "activation roots must share a filesystem"); }
  if (await exists(paths.failed)) throw new RestoreAuthorityError("unsafe_path", "failed quarantine path already exists");
  if (!await empty(paths.rollback)) throw new RestoreAuthorityError("unsafe_path", "rollback reservation is not empty");
  const initial: RestoreState = { format: "anamnesis.restore-state/1", operation_id: input.operationId, paths, phase: "STAGED_VERIFIED" };
  if (await exists(paths.state)) throw new RestoreAuthorityError("operation_locked", "restore journal already exists");
  await atomicJson(paths.state, initial);
  return activation(paths);
}

/** Only an external, trusted runtime adapter may provide this observation. This module never starts, stops, fences, or verifies a service itself. */
export interface TrustedRestoreVerification { source: "trusted-adapter"; healthy: boolean; }
export interface RestoreActivation { state(): Promise<RestoreState>; recover(): Promise<RestoreState>; renameLive(): Promise<RestoreState>; promoteStaging(): Promise<RestoreState>; markWillStart(): Promise<RestoreState>; markStarted(verification: TrustedRestoreVerification): Promise<RestoreState>; fencePromoted(): Promise<RestoreState>; quarantinePromoted(): Promise<RestoreState>; rollback(): Promise<RestoreState>; markRolledBack(healthy: boolean): Promise<RestoreState>; }
function activation(paths: RestorePaths): RestoreActivation {
  const read = async () => stateShape(JSON.parse(await (await import("node:fs/promises")).readFile(paths.state, "utf8")));
  const save = async (s: RestoreState) => { await atomicJson(paths.state, s); return s; };
  const check = async (s: RestoreState, phase: RestorePhase) => { if (s.paths.live !== paths.live || s.paths.staging !== paths.staging || s.paths.rollback !== paths.rollback || s.paths.failed !== paths.failed) throw new RestoreAuthorityError("unsafe_path", "journal paths differ"); if (s.phase !== phase) throw new RestoreAuthorityError("unsafe_path", `expected ${phase}, got ${s.phase}`); };
  const renameLive = async () => { const s = await read(); await check(s, "STAGED_VERIFIED"); if (!(await exists(paths.live) && await exists(paths.staging) && await empty(paths.rollback))) throw new RestoreAuthorityError("unsafe_path", "unknown STAGED_VERIFIED paths"); const w = await save({ ...s, phase: "WILL_RENAME_LIVE" }); await rename(paths.live, paths.rollback); await syncDirectory(dirname(paths.live)); return save({ ...w, phase: "LIVE_RENAMED" }); };
  const promote = async () => { const s = await read(); await check(s, "LIVE_RENAMED"); if (!(await exists(paths.rollback) && await exists(paths.staging) && !await exists(paths.live))) throw new RestoreAuthorityError("unsafe_path", "unknown LIVE_RENAMED paths"); const w = await save({ ...s, phase: "WILL_PROMOTE" }); await rename(paths.staging, paths.live); await syncDirectory(dirname(paths.live)); return save({ ...w, phase: "STAGING_PROMOTED" }); };
  const willStart = async () => { const s = await read(); await check(s, "STAGING_PROMOTED"); if (!(await exists(paths.live) && await exists(paths.rollback) && !await exists(paths.staging))) throw new RestoreAuthorityError("unsafe_path", "unknown STAGING_PROMOTED paths"); return save({ ...s, phase: "WILL_START" }); };
  const started = async (verification: TrustedRestoreVerification) => { const s = await read(); await check(s, "WILL_START"); if (verification.source !== "trusted-adapter") throw new RestoreAuthorityError("unsafe_path", "untrusted start result"); if (!verification.healthy) return save({ ...s, phase: "WILL_FENCE_PROMOTED" }); return save({ ...s, phase: "STARTED" }); };
  const fence = async () => { const s = await read(); await check(s, "WILL_FENCE_PROMOTED"); if (!(await exists(paths.live) && await exists(paths.rollback))) throw new RestoreAuthorityError("unsafe_path", "unknown promoted paths"); return save({ ...s, phase: "PROMOTED_FENCED" }); };
  const quarantine = async () => { const s = await read(); await check(s, "PROMOTED_FENCED"); if (!(await exists(paths.live) && await exists(paths.rollback) && !await exists(paths.failed))) throw new RestoreAuthorityError("unsafe_path", "unknown quarantine paths"); const w = await save({ ...s, phase: "WILL_QUARANTINE_PROMOTED" }); await rename(paths.live, paths.failed); await syncDirectory(dirname(paths.live)); return save({ ...w, phase: "PROMOTED_QUARANTINED" }); };
  const rollback = async () => { const s = await read(); await check(s, "PROMOTED_QUARANTINED"); if (!(await exists(paths.failed) && await exists(paths.rollback) && !await exists(paths.live))) throw new RestoreAuthorityError("unsafe_path", "unknown rollback paths"); const w = await save({ ...s, phase: "WILL_ROLLBACK" }); await rename(paths.rollback, paths.live); await syncDirectory(dirname(paths.live)); return save({ ...w, phase: "ROLLBACK_PROMOTED" }); };
  const rolledBack = async (healthy: boolean) => { const s = await read(); await check(s, "ROLLBACK_PROMOTED"); if (!healthy || !(await exists(paths.live) && await exists(paths.failed))) throw new RestoreAuthorityError("unsafe_path", "old live root is not verified"); return save({ ...s, phase: "ROLLED_BACK" }); };
  return { state: read, recover: read, renameLive, promoteStaging: promote, markWillStart: willStart, markStarted: started, fencePromoted: fence, quarantinePromoted: quarantine, rollback, markRolledBack: rolledBack };
}
