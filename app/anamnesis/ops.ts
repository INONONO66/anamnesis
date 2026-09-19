#!/usr/bin/env node
import { randomUUID } from "node:crypto";
import { lstat, readFile } from "node:fs/promises";
import { spawn } from "node:child_process";
import { join } from "node:path";
import { RpcClient } from "./client.ts";
import { runtimeRoot, socketPath, loadProviderConfig } from "./config.ts";
import { foreground } from "./daemon.ts";
import { ingestSource } from "./source.ts";
import { ingestAgentLog } from "./agentlog.ts";
import { ingestSlack } from "./slack.ts";
import { ingestNotion } from "./notion.ts";
import { ingestClaudeRaw } from "./clauderaw.ts";
import { ingestCodexRaw } from "./codexraw.ts";
import { ingestGjcRaw } from "./gjcraw.ts";
import { ingestOmoRaw } from "./omoraw.ts";
import { ingestMiscRaw } from "./miscraw.ts";
import { managed } from "./managed.ts";
import { fileURLToPath } from "node:url";
import { applyAuthorityEnvironment, offlineBackup, offlineRestore } from "./offline-ops.ts";

const ingestCommands = ["ingest", "ingest-agentlog", "ingest-slack", "ingest-notion", "ingest-claude-raw", "ingest-codex-raw", "ingest-gjc-raw", "ingest-omo-raw", "ingest-misc-raw"];
const usage = "usage: anamnesis-ops up|down|backup <destination-dir>|restore <archive-dir>|extract|embed|recall <query>|foreground|managed|status|verify|ingest <snapshot.jsonl> <checkpoint.json>";
const uuidv7 = () => { const value = randomUUID(); return `${value.slice(0, 14)}7${value.slice(15, 19)}8${value.slice(20)}`; };
const fail = (code: string, exit = 1): never => { console.log(JSON.stringify({ error: code })); process.exit(exit); throw new Error(code); };

async function connectClient(root: string): Promise<RpcClient> {
  const token = (await readFile(join(root, "token"), "utf8")).trim();
  return RpcClient.connect(process.env["ANAMNESIS_RUNTIME_SOCKET"] ?? socketPath(root), token);
}
async function main(): Promise<void> {
  const [command = "status", ...extra] = process.argv.slice(2);
  const valid = ["up", "down", "backup", "restore", "extract", "embed", "recall", "foreground", "managed", "status", "verify", ...ingestCommands];
  if (!valid.includes(command) || (ingestCommands.includes(command) ? extra.length !== 2 : command === "backup" || command === "restore" || command === "recall" ? extra.length !== 1 : extra.length)) throw new Error(usage);
  if (command === "foreground") { await foreground(); return; }
  if (command === "managed") { await managed(fileURLToPath(import.meta.url)); return; }
  const root = runtimeRoot(), socket = process.env["ANAMNESIS_RUNTIME_SOCKET"] ?? socketPath(root);
  await applyAuthorityEnvironment(root);
  if (command === "up") {
    let config;
    try { config = await loadProviderConfig(process.env); } catch { fail("extraction_provider_required", 2); }
    if (!config || !config.llm.baseUrl) fail("extraction_provider_required", 2);
    const child = spawn(process.execPath, [fileURLToPath(import.meta.url), "managed"], { env: process.env, stdio: "ignore", detached: true });
    child.unref();
    const deadline = Date.now() + 90000;
  while (Date.now() < deadline) { try { await lstat(socket); return; } catch (error) { const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown"; if (code !== "ENOENT") throw error; await new Promise(resolve => setImmediate(resolve)); } }
    fail("runtime_start_timeout");
  }
  if (command === "down") {
    try { const client = await connectClient(root); try { await client.request("shutdown", {}); } finally { await client.close(); } }
    catch (error) { const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown"; if (code !== "ENOENT" && code !== "ECONNREFUSED") throw error; }
    return;
  }
  if (command === "backup") {
    try { const owner = JSON.parse(await readFile(join(root, "owner", "owner.json"), "utf8")); if (owner.pid === process.pid || (Number.isSafeInteger(owner.pid) && (() => { try { process.kill(owner.pid, 0); return true; } catch { return false; } })())) fail("daemon_live"); } catch (error) { const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown"; if (code !== "ENOENT") throw error; }
  }
  if (command === "restore") {
    try { await lstat(socket); } catch (error) { const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown"; if (code === "ENOENT" || code === "ECONNREFUSED") { const result = await offlineRestore(root, extra[0]!); console.log(JSON.stringify({ state: "complete", operation_id: result.operation_id, manifest: { format: result.manifest.format, members: result.manifest.members.length }, container: result.container, uri: result.uri })); return; } throw error; }
  }
  let client;
  try { client = await connectClient(root); } catch (error) {
    const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown";
    if (command === "embed") { console.log(JSON.stringify({ embeddings: "disabled", drained: 0 })); return; }
    if (command === "backup" && (code === "ENOENT" || code === "ECONNREFUSED")) { const result = await offlineBackup(root, extra[0]!); console.log(JSON.stringify({ state: "complete", operation_id: result.operation_id, manifest: { format: result.manifest.format, members: result.manifest.members.length, objects: result.manifest.objects.length } })); return; }
    throw error;
  }
  try {
    if (ingestCommands.includes(command)) {
      const handlers: Record<string, (a: string, b: string, c: RpcClient) => Promise<void>> = { ingest: ingestSource, "ingest-agentlog": ingestAgentLog, "ingest-slack": ingestSlack, "ingest-notion": ingestNotion, "ingest-claude-raw": ingestClaudeRaw, "ingest-codex-raw": ingestCodexRaw, "ingest-gjc-raw": ingestGjcRaw, "ingest-omo-raw": ingestOmoRaw, "ingest-misc-raw": ingestMiscRaw };
      await handlers[command]!(extra[0]!, extra[1]!, client); return;
    }
    if (command === "backup") { console.log(JSON.stringify(await client.request("backup", { operation_id: uuidv7(), destination: extra[0]! }))); return; }
    if (command === "restore") { console.log(JSON.stringify(await client.request("restore", { operation_id: uuidv7(), archive: extra[0]! }))); return; }
    if (command === "embed") { const status = await client.request("status", {});
      if (!status.capabilities.embeddings) { console.log(JSON.stringify({ embeddings: "disabled", drained: 0 })); return; }
      console.log(JSON.stringify({ embeddings: "enabled", drained: 0 })); return;
    }
    if (command === "extract") { const status = await client.request("status", {}); if (!status.capabilities.extraction) fail("extraction_provider_required", 2); console.log(JSON.stringify({ extraction: "ready" })); return; }
    if (command === "recall") { console.log(JSON.stringify(await client.request("recall", { query: extra[0]!, limit: 10 }))); return; }
    const status = await client.request("status", {});
    if (command === "status") { console.log(JSON.stringify(status)); return; }
    if (command === "verify") {
      const checks = { root_private: ((await lstat(root)).mode & 0o777) === 0o700, token_private: ((await lstat(join(root, "token"))).mode & 0o777) === 0o600, socket_private: ((await lstat(socket)).mode & 0o777) === 0o600, authenticated: true, database_writer_fence: status.capabilities.writer_fence === "database", storage_available: status.storage === "available", spool_clean: status.spool.quarantined === 0 && status.spool.blocked === 0 };
      const ok = Object.values(checks).every(Boolean); console.log(JSON.stringify({ ok, scope: "runtime-admission-health; not a full database integrity audit", checks, status })); if (!ok) process.exitCode = 1;
    }
  } finally { await client.close(); }
}
main().catch(error => { const code = error && typeof error === "object" && "code" in error ? String(error.code) : "unknown"; console.error(JSON.stringify({ event: "operation_failed", code, error: String(error) })); process.exitCode = 1; });
