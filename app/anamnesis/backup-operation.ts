import { createHash, randomBytes } from "node:crypto";
import { constants, type BigIntStats } from "node:fs";
import fs from "node:fs/promises";
import { dirname, join, resolve } from "node:path";
import { atomicJson, hasCode, syncDirectory } from "./config.ts";
import { parseArchiveCompletion, parseArchiveManifest } from "./archive-manifest.ts";

/** Control journal only; not a backup executor, data authority, RPC, or writer fence.
 * The future orchestrator must retain exclusive custody of source/destination
 * paths and obey this sibling operation lock (including restore). Node has no
 * portable openat: inode checks do NOT defend against a malicious same-UID writer.
 *
 * PREPARE is write-ahead intent, persisted before the orchestrator reserves the
 * nonexistent destination. This module never creates/copies/publishes an archive,
 * stops/starts a DB, clears the journal, or retries an unknown external effect.
 * The live-root backup.state is the sole control record here. A destination-side
 * journal, gate/object leases, runtime integration and terminal clearing protocol
 * belong to the future orchestrator; none is implicitly supplied by this API.
 */
export const BACKUP_OPERATION_LIMITS = Object.freeze({ body_bytes: 8192, command_bytes: 4096, state_bytes: 65536, proof_bytes: 1024 * 1024, history: 10, json_depth: 12 });
export type BackupPhase = "PREPARE" | "CUTOFF" | "STOPPING" | "DB_STOPPED" | "DUMPED" | "DB_STARTED" | "COPYING" | "COMPLETE" | "FAILED";
export type BackupIntent = "prepare_destination_and_cutoff" | "stop_database" | "dump_database" | "start_database" | "copy_archive" | "publish_complete" | null;
export type BackupProofKind = "cutoff" | "database_stopped" | "dump_verified" | "database_healthy" | "archive_verified" | "completion_published";
export interface BackupBody {
  format: "anamnesis.backup-operation/1"; operation_id: string;
  source: { path: string; dev: string; ino: string };
  destination: { path: string; parent_dev: string; parent_ino: string };
}
export interface BackupProof { kind: BackupProofKind; path: string; bytes: number; sha256: string }
export interface BackupFailure { code: "io_error" | "validation_error" | "cancelled" | "orchestrator_error"; effect: "not_run" | "unknown" }
export interface BackupTransition {
  expected_version: number; expected_phase: BackupPhase; phase: BackupPhase;
  intent: BackupIntent; proof: BackupProof | null; failure: BackupFailure | null;
}
export interface BackupState {
  format: "anamnesis.backup-state/1"; body: BackupBody; body_sha256: string;
  phase: BackupPhase; intent: BackupIntent; version: number;
  destination_identity: { dev: string; ino: string } | null;
  history: BackupTransition[];
}
export interface BackupRecovery {
  action: "inspect_database_before_gate_release" | "inspect_archive_before_resume" | "inspect_publication" | "none";
  failed_from: BackupPhase | null; preserve_dump: boolean;
  /** Historical health evidence never authorizes releasing a gate after restart. */
  may_release_gate: false;
}
export type BackupStatus = { status: "unknown"; operation_id: string | null; reason: "not_found" | "different_operation" | "interrupted_write" | "indeterminate" } |
  { status: "pending" | "terminal"; operation_id: string; state: BackupState; recovery: BackupRecovery };
export type BackupOperationCode = "invalid_body" | "invalid_transition" | "unsafe_path" | "identity_conflict" | "cas_conflict" | "destination_exists" |
  "restore_pending" | "operation_locked" | "stale_lock" | "stale_state" | "ownership_lost" | "proof_required" | "invalid_proof" | "outcome_unknown";
export class BackupOperationError extends Error {
  readonly code: BackupOperationCode;
  constructor(code: BackupOperationCode, detail: string, options?: ErrorOptions) { super(`${code}: ${detail}`, options); this.name = "BackupOperationError"; this.code = code; }
}
export interface BackupProofContext {
  body: BackupBody; state: BackupState; transition: BackupTransition; report: Buffer;
}
export interface BackupOperationOptions {
  /** TRUST BOUNDARY: required for every observation, never for intent alone.
   * Throw unless the report establishes the transition for this exact operation,
   * body digest, expected version, source inode, reserved destination and cutoff.
   * cutoff: gate + all writer fences, spool_pending=0, full cutoff manifest/pins.
   * database_stopped: actual offline observation of the pinned service/volume.
   * dump_verified: successful offline admin result, exact image, fsynced dump and
   * validated metadata bound to cutoff. database_healthy: actual restart/health.
   * archive_verified: full manifest/member/dump validation and destination fsync.
   * completion_published: exact verified manifest, marker-last + parent fsync.
   *
   * The journal verifies report bytes/hash, NOT their truth. Report schemas and
   * authenticity are this trusted adapter's responsibility, not caller booleans.
   * Reports must be immutable and retained. Do not expose this callback to RPC
   * callers, use a no-op validator, or treat synthetic test evidence as real DB
   * qualification. Callback failure cannot change the saved journal.
   */
  validateProof?: (context: BackupProofContext) => Promise<void>;
}
export interface BackupOperation {
  begin(body: string | Uint8Array): Promise<BackupStatus>;
  advance(body: string | Uint8Array, transition: string | Uint8Array): Promise<BackupStatus>;
  status(operationId: string): Promise<BackupStatus>;
  recover(): Promise<BackupStatus>;
  release(): Promise<void>;
}
const UUID7 = /^[0-9a-f]{8}-[0-9a-f]{4}-7[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/;
const HEX = /^[0-9a-f]{64}$/;
const DECIMAL = /^(0|[1-9][0-9]{0,19})$/;
const PHASES: readonly string[] = ["PREPARE", "CUTOFF", "STOPPING", "DB_STOPPED", "DUMPED", "DB_STARTED", "COPYING", "COMPLETE", "FAILED"];
const INTENTS: readonly (string | null)[] = [null, "prepare_destination_and_cutoff", "stop_database", "dump_database", "start_database", "copy_archive", "publish_complete"];
const PROOFS: readonly string[] = ["cutoff", "database_stopped", "dump_verified", "database_healthy", "archive_verified", "completion_published"];
function need(test: unknown, code: BackupOperationCode, detail: string): asserts test { if (!test) throw new BackupOperationError(code, detail); }
function record(value: unknown, keys: string[], code: BackupOperationCode): Record<string, unknown> {
  need(value !== null && typeof value === "object" && !Array.isArray(value), code, "expected object");
  const obj = value as Record<string, unknown>;
  need(Object.keys(obj).length === keys.length && keys.every(k => Object.hasOwn(obj, k)), code, "missing or unknown field"); return obj;
}
function text(value: unknown, pattern: RegExp): value is string { return typeof value === "string" && pattern.test(value); }
function canonical(value: unknown): string {
  if (Array.isArray(value)) return `[${value.map(canonical).join(",")}]`;
  if (value !== null && typeof value === "object") { const obj = value as Record<string, unknown>; return `{${Object.keys(obj).sort().map(k => `${JSON.stringify(k)}:${canonical(obj[k])}`).join(",")}}`; }
  return JSON.stringify(value);
}
function hash(value: string | Uint8Array): string { return createHash("sha256").update(value).digest("hex"); }
function decode(bytes: string | Uint8Array, max: number, code: BackupOperationCode): unknown {
  try {
    need(typeof bytes === "string" || bytes instanceof Uint8Array, code, "expected UTF-8 bytes");
    need(Buffer.byteLength(bytes) <= max, code, "byte limit");
    const raw = typeof bytes === "string" ? bytes : new TextDecoder("utf-8", { fatal: true, ignoreBOM: true }).decode(bytes);
    let depth = 0, quoted = false;
    for (let i = 0; i < raw.length; i++) {
      if (quoted && raw[i] === "\\") { i++; continue; }
      if (raw[i] === '"') quoted = !quoted;
      else if (!quoted && (raw[i] === "{" || raw[i] === "[")) need(++depth <= BACKUP_OPERATION_LIMITS.json_depth, code, "depth limit");
      else if (!quoted && (raw[i] === "}" || raw[i] === "]")) depth--;
    }
    const value: unknown = JSON.parse(raw);
    // Requiring canonical bytes also rejects duplicate keys, overflow, BOM,
    // alternate numeric spellings and unknown whitespace without lossy identity.
    need(canonical(value) === raw, code, "noncanonical JSON"); return value;
  } catch (cause) { if (cause instanceof BackupOperationError) throw cause; throw new BackupOperationError(code, "invalid JSON/UTF-8", { cause }); }
}
function safeAbsolute(path: unknown): asserts path is string {
  need(typeof path === "string" && path.length <= 1024 && path.startsWith("/") && path !== "/" && resolve(path) === path && !/[^\x20-\x7e]|[\\%:]/.test(path), "unsafe_path", "canonical absolute POSIX path required");
}
export function parseBackupOperationBody(bytes: string | Uint8Array): BackupBody {
  const b = record(decode(bytes, BACKUP_OPERATION_LIMITS.body_bytes, "invalid_body"), ["format", "operation_id", "source", "destination"], "invalid_body");
  need(b.format === "anamnesis.backup-operation/1" && text(b.operation_id, UUID7), "invalid_body", "format or UUIDv7");
  const s = record(b.source, ["path", "dev", "ino"], "invalid_body"), d = record(b.destination, ["path", "parent_dev", "parent_ino"], "invalid_body");
  safeAbsolute(s.path); safeAbsolute(d.path);
  need([s.dev, s.ino, d.parent_dev, d.parent_ino].every(v => text(v, DECIMAL)) && s.ino !== "0" && d.parent_ino !== "0", "invalid_body", "filesystem identity");
  need(s.path !== d.path && !s.path.startsWith(d.path + "/") && !d.path.startsWith(s.path + "/") && !d.path.startsWith(s.path + "-"), "unsafe_path", "overlapping or reserved destination");
  return b as unknown as BackupBody;
}
function parseTransition(value: unknown, operationId: string): BackupTransition {
  const c = record(value, ["expected_version", "expected_phase", "phase", "intent", "proof", "failure"], "invalid_transition");
  need(typeof c.expected_version === "number" && Number.isSafeInteger(c.expected_version) && c.expected_version >= 1 && c.expected_version < BACKUP_OPERATION_LIMITS.history &&
    typeof c.expected_phase === "string" && PHASES.includes(c.expected_phase) && typeof c.phase === "string" && PHASES.includes(c.phase) && INTENTS.includes(c.intent as string | null), "invalid_transition", "phase/version/intent");
  if (c.proof !== null) {
    const p = record(c.proof, ["kind", "path", "bytes", "sha256"], "invalid_proof");
    need(typeof p.kind === "string" && PROOFS.includes(p.kind) && text(p.sha256, HEX) && typeof p.bytes === "number" && Number.isSafeInteger(p.bytes) && p.bytes > 0 && p.bytes <= BACKUP_OPERATION_LIMITS.proof_bytes &&
      typeof p.path === "string" && p.path.startsWith(`backup-evidence/${operationId}/`) && /^[a-z0-9][a-z0-9._-]{0,127}\.json$/.test(p.path.slice(`backup-evidence/${operationId}/`.length)), "invalid_proof", "bounded owned report reference required");
  }
  if (c.failure !== null) {
    const f = record(c.failure, ["code", "effect"], "invalid_transition");
    need(typeof f.code === "string" && ["io_error", "validation_error", "cancelled", "orchestrator_error"].includes(f.code) && typeof f.effect === "string" && ["not_run", "unknown"].includes(f.effect), "invalid_transition", "failure classification");
  }
  return c as unknown as BackupTransition;
}
/** The only legal edges. Intent is durable before any external side effect. */
function edge(previous: BackupPhase, intent: BackupIntent, c: BackupTransition): void {
  need(c.expected_phase === previous && previous !== "COMPLETE" && previous !== "FAILED", "cas_conflict", "phase is not current or is terminal");
  if (c.phase === "FAILED") { need(c.failure !== null && c.proof === null && c.intent === null, "invalid_transition", "failure must retain unknown/not-run distinction"); return; }
  need(c.failure === null, "invalid_transition", "failure on nonfailure transition");
  let expected: [BackupPhase, BackupIntent, BackupProofKind | null] | undefined;
  switch (previous) {
    case "PREPARE": expected = ["CUTOFF", null, "cutoff"]; break;
    case "CUTOFF": expected = ["STOPPING", "stop_database", null]; break;
    case "STOPPING": expected = ["DB_STOPPED", "dump_database", "database_stopped"]; break;
    case "DB_STOPPED": expected = ["DUMPED", "start_database", "dump_verified"]; break;
    case "DUMPED": expected = ["DB_STARTED", null, "database_healthy"]; break;
    case "DB_STARTED": expected = ["COPYING", "copy_archive", null]; break;
    case "COPYING": expected = intent === "copy_archive" ? ["COPYING", "publish_complete", "archive_verified"] : ["COMPLETE", null, "completion_published"]; break;
  }
  need(expected && c.phase === expected[0] && c.intent === expected[1] && (c.proof?.kind ?? null) === expected[2], "invalid_transition", "missing proof or illegal phase/intent edge");
}
export function parseBackupOperationState(bytes: string | Uint8Array): BackupState {
  const s = record(decode(bytes, BACKUP_OPERATION_LIMITS.state_bytes, "stale_state"), ["format", "body", "body_sha256", "phase", "intent", "version", "destination_identity", "history"], "stale_state");
  const body = parseBackupOperationBody(canonical(s.body));
  need(s.format === "anamnesis.backup-state/1" && s.body_sha256 === hash(canonical(body)) && Array.isArray(s.history) && s.history.length < BACKUP_OPERATION_LIMITS.history, "stale_state", "format/digest/history");
  let phase: BackupPhase = "PREPARE", intent: BackupIntent = "prepare_destination_and_cutoff", version = 1;
  for (const value of s.history) { const c = parseTransition(value, body.operation_id); need(c.expected_version === version, "stale_state", "noncontiguous history"); edge(phase, intent, c); phase = c.phase; intent = c.intent; version++; }
  need(s.phase === phase && s.intent === intent && s.version === version, "stale_state", "derived phase/version disagreement");
  const hasCutoff = (s.history as BackupTransition[]).some(c => c.phase === "CUTOFF");
  if (hasCutoff) { const d = record(s.destination_identity, ["dev", "ino"], "stale_state"); need(text(d.dev, DECIMAL) && text(d.ino, DECIMAL) && d.ino !== "0", "stale_state", "destination identity"); }
  else need(s.destination_identity === null, "stale_state", "destination identity before observation");
  return s as unknown as BackupState;
}
function same(a: BigIntStats, b: BigIntStats): boolean { return a.dev === b.dev && a.ino === b.ino && a.mode === b.mode && a.uid === b.uid && a.nlink === b.nlink && a.size === b.size && a.mtimeNs === b.mtimeNs && a.ctimeNs === b.ctimeNs; }
async function directory(path: string): Promise<BigIntStats> {
  const s = await fs.lstat(path, { bigint: true });
  need(s.isDirectory() && !s.isSymbolicLink() && s.uid === BigInt(process.getuid!()) && (s.mode & 0o022n) === 0n && await fs.realpath(path) === path, "unsafe_path", "owned real non-shared directory required"); return s;
}
async function readOwned(path: string, max: number, sync = false): Promise<Buffer> {
  await directory(dirname(path));
  const before = await fs.lstat(path, { bigint: true });
  need(before.isFile() && before.nlink === 1n && before.uid === BigInt(process.getuid!()) && (before.mode & 0o022n) === 0n && before.size <= BigInt(max), "unsafe_path", "bounded single-link owned file required");
  const file = await fs.open(path, constants.O_RDONLY | constants.O_NOFOLLOW | constants.O_NONBLOCK);
  try {
    need(same(before, await file.stat({ bigint: true })), "stale_state", "file changed before open");
    const bytes = Buffer.alloc(Number(before.size) + 1); let offset = 0;
    while (offset < bytes.length) { const { bytesRead } = await file.read(bytes, offset, bytes.length - offset, null); if (bytesRead === 0) break; offset += bytesRead; }
    need(BigInt(offset) === before.size && same(before, await file.stat({ bigint: true })) && same(before, await fs.lstat(path, { bigint: true })), "stale_state", "file changed during read");
    if (sync) await file.sync(); return bytes.subarray(0, offset);
  } finally { await file.close(); }
}
async function exists(path: string): Promise<boolean> { try { await fs.lstat(path); return true; } catch (cause) { if (hasCode(cause, "ENOENT")) return false; throw cause; } }
function recovery(state: BackupState): BackupRecovery {
  const failed = state.phase === "FAILED", phase = failed ? state.history.at(-1)!.expected_phase : state.phase;
  const previousIntent = failed ? state.history.at(-2)?.intent ?? "prepare_destination_and_cutoff" : state.intent;
  return { action: phase === "COMPLETE" ? "none" : phase === "COPYING" && previousIntent === "publish_complete" ? "inspect_publication" : phase === "DB_STARTED" || phase === "COPYING" ? "inspect_archive_before_resume" : "inspect_database_before_gate_release",
    failed_from: failed ? phase : null, preserve_dump: state.history.some(c => c.phase === "DUMPED"), may_release_gate: false };
}
function result(state: BackupState): BackupStatus { return { status: state.phase === "COMPLETE" || state.phase === "FAILED" ? "terminal" : "pending", operation_id: state.body.operation_id, state, recovery: recovery(state) }; }

/** Exclusive lock with conservative PID reuse protection. No time-based stealing.
 * A dead, fully formed owner can be reclaimed under the acquisition guard; an
 * incomplete owner/guard or unexpected entry is operator repair, never guessed.
 */
export async function acquireBackupOperation(root: string, options: BackupOperationOptions = {}): Promise<BackupOperation> {
  safeAbsolute(root); const rootStat = await directory(root); await directory(dirname(root));
  const lock = root + "-operation.lock", claim = lock + ".claim", ownerPath = join(lock, "owner.json"), path = join(root, "backup.state");
  const owner = { format: "anamnesis.operation-lock/1", pid: process.pid, nonce: randomBytes(32).toString("hex"), root };
  const ownerBytes = canonical(owner);
  try { await fs.mkdir(claim, { mode: 0o700 }); }
  catch (cause) { throw new BackupOperationError(hasCode(cause, "EEXIST") ? "stale_lock" : "outcome_unknown", "acquisition guard unavailable; no timed reclamation", { cause }); }
  try {
    if (await exists(lock)) {
      let old: Record<string, unknown>;
      try {
        await directory(lock);
        const entries = await fs.readdir(lock); need(entries.length === 1 && entries[0] === "owner.json", "stale_lock", "incomplete or foreign lock entries");
        old = record(decode((await readOwned(ownerPath, 4096)).toString("utf8").replace(/\n$/, ""), 4096, "stale_lock"), ["format", "pid", "nonce", "root"], "stale_lock");
        need(old.format === owner.format && old.root === root && typeof old.pid === "number" && Number.isSafeInteger(old.pid) && old.pid > 0 && old.pid <= 2147483647 && text(old.nonce, HEX), "stale_lock", "invalid owner");
      } catch (cause) { throw new BackupOperationError("stale_lock", "owner cannot be proven", { cause }); }
      let dead = false;
      try { process.kill(old.pid as number, 0); } catch (cause) { if (hasCode(cause, "ESRCH")) dead = true; }
      need(dead, "operation_locked", "live or inaccessible PID is conservatively protected");
      // All acquisitions hold claim. Never recursively remove a lock tree.
      await fs.unlink(ownerPath); await fs.rmdir(lock); await syncDirectory(dirname(root));
    }
    await fs.mkdir(lock, { mode: 0o700 });
    await atomicJson(ownerPath, JSON.parse(ownerBytes)); await syncDirectory(dirname(root));
  } finally { await fs.rmdir(claim); await syncDirectory(dirname(root)); }
  let released = false, uncertain = false;
  // Serialize calls on this owned handle too. Rejection is delivered to its caller
  // while the queue's continuation remains usable for explicit reconciliation.
  let tail: Promise<unknown> = Promise.resolve();
  const serial = <T>(run: () => Promise<T>): Promise<T> => { const next = tail.then(run, run); tail = next; return next; };
  const assertOwned = async () => {
    need(!released, "ownership_lost", "released handle");
    const current = await directory(root); need(current.dev === rootStat.dev && current.ino === rootStat.ino, "ownership_lost", "source root replaced");
    need((await readOwned(ownerPath, 4096)).toString("utf8").replace(/\n$/, "") === ownerBytes, "ownership_lost", "owner changed");
  };
  const validateBody = async (body: BackupBody) => {
    need(body.source.path === root && body.source.dev === String(rootStat.dev) && body.source.ino === String(rootStat.ino), "identity_conflict", "source identity differs");
    const parent = await directory(dirname(body.destination.path));
    need(String(parent.dev) === body.destination.parent_dev && String(parent.ino) === body.destination.parent_ino, "identity_conflict", "destination parent identity differs");
  };
  const load = async (stabilize = false): Promise<BackupState | null> => {
    if (!await exists(path)) return null;
    const raw = await readOwned(path, BACKUP_OPERATION_LIMITS.state_bytes + 1, stabilize);
    need(raw.at(-1) === 10, "stale_state", "missing journal framing newline");
    const state = parseBackupOperationState(raw.subarray(0, -1)); await validateBody(state.body);
    if (state.destination_identity) {
      const dest = await directory(state.body.destination.path);
      need(state.destination_identity.dev === String(dest.dev) && state.destination_identity.ino === String(dest.ino), "stale_state", "destination replaced");
    }
    if (stabilize) await syncDirectory(root); return state;
  };
  const interrupted = async () => (await fs.readdir(root)).some(name => /^backup\.state\.[0-9a-f-]{36}\.tmp$/.test(name));
  const persist = async (state: BackupState): Promise<BackupStatus> => {
    await assertOwned();
    try { await atomicJson(path, JSON.parse(canonical(state))); }
    catch (cause) { uncertain = true; throw new BackupOperationError("outcome_unknown", "write may have renamed; recover before any further transition", { cause }); }
    return result(state);
  };
  const readStatus = async (operationId: string | null, reconcile: boolean): Promise<BackupStatus> => {
    await assertOwned();
    if (uncertain && !reconcile) return { status: "unknown", operation_id: operationId, reason: "indeterminate" };
    try {
      const state = await load(true);
      if (!state) {
        const pending = await interrupted(); uncertain = pending;
        return { status: "unknown", operation_id: operationId, reason: pending ? "interrupted_write" : "not_found" };
      }
      uncertain = false;
      if (operationId !== null && operationId !== state.body.operation_id) return { status: "unknown", operation_id: operationId, reason: "different_operation" };
      return result(state);
    } catch (cause) {
      // Only journal/path/I/O uncertainty is a status UNKNOWN. No fallback to
      // success and no filesystem repair. The mutation APIs retain typed errors.
      if (!(cause instanceof BackupOperationError) && !(cause instanceof Error && "code" in cause)) throw cause;
      uncertain = true; return { status: "unknown", operation_id: operationId, reason: "indeterminate" };
    }
  };
  return {
    begin: bytes => serial(async () => {
      const body = parseBackupOperationBody(bytes); await assertOwned(); await validateBody(body);
      need(!uncertain, "stale_state", "recover an uncertain journal first");
      const state = await load();
      if (state) { need(canonical(state.body) === canonical(body), "identity_conflict", "another operation or changed canonical body"); return readStatus(body.operation_id, false); }
      need(!await interrupted(), "stale_state", "interrupted initial journal; no automatic temp promotion/deletion");
      need(!await exists(root + "-restore.state"), "restore_pending", "restore journal exists");
      need(!await exists(body.destination.path), "destination_exists", "complete/partial destination may not be reused");
      return persist({ format: "anamnesis.backup-state/1", body, body_sha256: hash(canonical(body)), phase: "PREPARE", intent: "prepare_destination_and_cutoff", version: 1, destination_identity: null, history: [] });
    }),
    advance: (bytes, commandBytes) => serial(async () => {
      const body = parseBackupOperationBody(bytes), c = parseTransition(decode(commandBytes, BACKUP_OPERATION_LIMITS.command_bytes, "invalid_transition"), body.operation_id);
      await assertOwned(); await validateBody(body); need(!uncertain, "stale_state", "recover an uncertain journal first");
      const state = await load(); need(state, "stale_state", "no persisted operation");
      need(canonical(state.body) === canonical(body), "identity_conflict", "another operation or changed canonical body");
      const prior = state.history.find(entry => entry.expected_version === c.expected_version);
      if (prior) { need(canonical(prior) === canonical(c), "cas_conflict", "version already bound to different command bytes"); return readStatus(body.operation_id, false); }
      need(c.expected_version === state.version, "cas_conflict", "expected version is stale/future"); edge(state.phase, state.intent, c);
      need(!await exists(root + "-restore.state"), "restore_pending", "restore journal exists");
      let destinationIdentity = state.destination_identity;
      if (c.proof) {
        need(options.validateProof, "proof_required", "trusted observation validator is mandatory");
        // Check every evidence directory, not merely the final parent.
        await directory(join(root, "backup-evidence")); await directory(join(root, "backup-evidence", body.operation_id));
        const report = await readOwned(join(root, c.proof.path), BACKUP_OPERATION_LIMITS.proof_bytes);
        need(report.length === c.proof.bytes && hash(report) === c.proof.sha256, "invalid_proof", "evidence bytes/hash differ");
        const dest = await directory(body.destination.path);
        if (c.phase === "CUTOFF") destinationIdentity = { dev: String(dest.dev), ino: String(dest.ino) };
        await options.validateProof({ body: structuredClone(body), state: structuredClone(state), transition: structuredClone(c), report: Buffer.from(report) });
        // Validator cannot mutate our inputs or swap observed paths unnoticed.
        const after = await directory(body.destination.path);
        need(after.dev === dest.dev && after.ino === dest.ino && hash(await readOwned(join(root, c.proof.path), BACKUP_OPERATION_LIMITS.proof_bytes, true)) === c.proof.sha256, "invalid_proof", "validated paths changed");
        await syncDirectory(dirname(join(root, c.proof.path))); await syncDirectory(join(root, "backup-evidence")); await syncDirectory(root);
        if (c.phase === "COMPLETE") {
          const marker = parseArchiveCompletion(await readOwned(join(body.destination.path, "backup.complete"), 4096));
          const manifest = await readOwned(join(body.destination.path, "manifest.json"), 8 * 1024 ** 2);
          const parsed = parseArchiveManifest(manifest);
          need(marker.operation_id === body.operation_id && parsed.operation_id === body.operation_id && marker.manifest_sha256 === hash(manifest) && marker.manifest_bytes === manifest.length, "invalid_proof", "completion does not bind this operation's manifest");
        }
      }
      // Re-read CAS after the trusted callback before publication. Callback must
      // not call this handle recursively or mutate the journal/control namespace.
      await assertOwned(); need(canonical(await load()) === canonical(state), "cas_conflict", "journal changed during validation");
      return persist({ ...state, phase: c.phase, intent: c.intent, version: state.version + 1, destination_identity: destinationIdentity, history: [...state.history, c] });
    }),
    status: operationId => serial(async () => { need(text(operationId, UUID7), "invalid_body", "UUIDv7 operation identity required"); return readStatus(operationId, false); }),
    recover: () => serial(() => readStatus(null, true)),
    release: () => serial(async () => {
      if (released) return; await assertOwned();
      const entries = await fs.readdir(lock); need(entries.length === 1 && entries[0] === "owner.json", "stale_lock", "unrelated lock entries preserved");
      await fs.unlink(ownerPath); await fs.rmdir(lock); released = true; await syncDirectory(dirname(root));
    }),
  };
}
