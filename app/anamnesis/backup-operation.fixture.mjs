import assert from "node:assert/strict";
import fs from "node:fs/promises";
import { createHash } from "node:crypto";
import { tmpdir } from "node:os";
import { join, dirname } from "node:path";
import { fork } from "node:child_process";
import { once } from "node:events";
import { syncBuiltinESMExports } from "node:module";
import { fileURLToPath } from "node:url";

export const api = () => import(process.env.BACKUP_OPERATION_MODULE ?? "./backup-operation.ts");
export const canonical = v => Array.isArray(v) ? `[${v.map(canonical).join(",")}]` : v !== null && typeof v === "object" ? `{${Object.keys(v).sort().map(k => `${JSON.stringify(k)}:${canonical(v[k])}`).join(",")}}` : JSON.stringify(v);
export const hash = v => createHash("sha256").update(v).digest("hex");
export const id = "01993000-0000-7000-8000-000000000001";
export const secondId = "01993000-0000-7000-8000-000000000002";
export const validation = async ({ report }) => {
  // Synthetic evidence validator, NOT a real DB, dump, fence or fsync proof.
  assert.deepEqual(JSON.parse(report), { fixture_observation: "validated-artifact-not-a-success-flag" });
};
export async function fixture(t) {
  const owner = await fs.realpath(await fs.mkdtemp(join(tmpdir(), "g005-backup-operation-")));
  const disposers = [];
  t.after(async () => { for (const dispose of disposers.reverse()) await dispose(); await fs.rm(owner, { recursive: true }); await assert.rejects(fs.lstat(owner), { code: "ENOENT" }); t.diagnostic(JSON.stringify({ cleanup: owner, absent: true })); });
  const root = join(owner, "live"), destination = join(owner, "archive");
  await fs.mkdir(root, { mode: 0o700 });
  await fs.writeFile(join(root, "unrelated"), "preserved", { mode: 0o600 });
  const source = await fs.lstat(root, { bigint: true }), parent = await fs.lstat(owner, { bigint: true });
  const body = canonical({ format: "anamnesis.backup-operation/1", operation_id: id,
    source: { path: root, dev: String(source.dev), ino: String(source.ino) },
    destination: { path: destination, parent_dev: String(parent.dev), parent_ino: String(parent.ino) } });
  return { owner, root, destination, body, disposers, state: join(root, "backup.state"), lock: root + "-operation.lock" };
}
export function command(version, previous, phase, intent = null, proof = null, failure = null) {
  return canonical({ expected_version: version, expected_phase: previous, phase, intent, proof, failure });
}
export async function proof(f, version, kind) {
  const path = `backup-evidence/${id}/${version}-${kind}.json`, bytes = canonical({ fixture_observation: "validated-artifact-not-a-success-flag" });
  await fs.mkdir(dirname(join(f.root, path)), { recursive: true, mode: 0o700 });
  await fs.writeFile(join(f.root, path), bytes, { mode: 0o600 });
  return { kind, path, bytes: Buffer.byteLength(bytes), sha256: hash(bytes) };
}
export async function advanceTo(f, journal, target) {
  let result = await journal.begin(f.body);
  const steps = [
    ["CUTOFF", null, "cutoff"], ["STOPPING", "stop_database", null],
    ["DB_STOPPED", "dump_database", "database_stopped"], ["DUMPED", "start_database", "dump_verified"],
    ["DB_STARTED", null, "database_healthy"], ["COPYING", "copy_archive", null],
    ["COPYING", "publish_complete", "archive_verified"],
  ];
  if (target === "PREPARE") return result;
  await fs.mkdir(f.destination, { mode: 0o700 }); // Future orchestrator's reservation, not backup execution.
  for (const [phase, intent, kind] of steps) {
    const p = kind ? await proof(f, result.state.version, kind) : null;
    result = await journal.advance(f.body, command(result.state.version, result.state.phase, phase, intent, p));
    if (phase === target && !(target === "PUBLISH")) return result;
  }
  return result;
}
export async function child(_t, f, mode = "hold", crashStage = "") {
  const worker = fork(fileURLToPath(import.meta.url), ["child", f.root, f.body, mode, crashStage], { stdio: ["ignore", "pipe", "pipe", "ipc"], env: process.env });
  let output = "";
  worker.stdout.on("data", b => { output += b; }); worker.stderr.on("data", b => { output += b; });
  const closed = once(worker, "close", { signal: AbortSignal.timeout(10000) });
  f.disposers.push(async () => { if (worker.exitCode === null && worker.signalCode === null) worker.kill("SIGKILL"); await closed; });
  const wait = event => new Promise((resolve, reject) => {
    const timer = setTimeout(() => { cleanup(); reject(new Error(`missing child event ${event}: ${output}`)); }, 10000);
    const onMessage = message => { if (message.event === event || message.event === "error") { cleanup(); message.event === "error" ? reject(new Error(JSON.stringify(message))) : resolve(message); } };
    const onExit = () => { cleanup(); reject(new Error(`child closed before ${event}: ${output}`)); };
    const cleanup = () => { clearTimeout(timer); worker.off("message", onMessage); worker.off("close", onExit); };
    worker.on("message", onMessage); worker.once("close", onExit);
  });
  return { worker, wait, closed, output: () => output };
}

if (process.argv[2] === "child") {
  const [, , , root, body, mode, crashStage] = process.argv;
  // Wait for parent subscription before any acquisition/persistence event.
  process.once("message", async () => {
    try {
      const { acquireBackupOperation } = await api();
      const journal = await acquireBackupOperation(root, { validateProof: validation });
      if (mode === "crash") {
        const originalOpen = fs.open, originalRename = fs.rename;
        fs.open = async (...args) => {
          const file = await originalOpen(...args);
          if (typeof args[0] === "string" && args[0].startsWith(join(root, "backup.state.")) && args[1] === "wx") {
            const sync = file.sync.bind(file);
            file.sync = async () => { await sync(); if (crashStage === "file-sync") { process.send({ event: "persisted", stage: crashStage }); process.channel?.ref(); await new Promise(() => {}); } };
          }
          return file;
        };
        fs.rename = async (...args) => { await originalRename(...args); if (args[1] === join(root, "backup.state") && crashStage === "rename") { process.send({ event: "persisted", stage: crashStage }); process.channel?.ref(); await new Promise(() => {}); } };
        syncBuiltinESMExports();
      }
      const result = mode === "phase" ? await advanceTo({ root, body, destination: JSON.parse(body).destination.path }, journal, crashStage) : await journal.begin(body);
      process.send({ event: mode === "crash" || mode === "phase" ? "persisted" : "ready", stage: mode === "phase" ? crashStage : "ack", result });
      process.on("message", async message => {
        try {
          if (message === "retry") process.send({ event: "retried", result: await journal.begin(body) });
          if (message === "release") { await journal.release(); process.send({ event: "released" }); process.disconnect(); }
        } catch (error) { process.send({ event: "error", code: error.code, message: error.message }); }
      });
    } catch (error) { process.send({ event: "error", code: error.code, message: error.message }); process.disconnect(); }
  });
}
