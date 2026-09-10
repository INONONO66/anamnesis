#!/usr/bin/env node
import { lstat, readFile } from "node:fs/promises";
import { join } from "node:path";
import { RpcClient } from "./client.ts";
import { runtimeRoot, socketPath } from "./config.ts";
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

async function main(): Promise<void> {
  const [command = "status", ...extra] = process.argv.slice(2);
  if (["ingest", "ingest-agentlog", "ingest-slack", "ingest-notion", "ingest-claude-raw", "ingest-codex-raw", "ingest-gjc-raw", "ingest-omo-raw", "ingest-misc-raw"].includes(command) ? extra.length !== 2 : extra.length || !["foreground", "managed", "status", "verify"].includes(command)) throw new Error("usage: anamnesis-ops foreground|managed|status|verify|ingest <snapshot.jsonl> <checkpoint.json>|ingest-agentlog|ingest-slack|ingest-notion|ingest-claude-raw|ingest-codex-raw|ingest-gjc-raw|ingest-omo-raw|ingest-misc-raw <export-directory> <checkpoint.json>");
  if (command === "foreground") { await foreground(); return; }
  if (command === "managed") { await managed(fileURLToPath(import.meta.url)); return; }
  const root = runtimeRoot();
  const token = (await readFile(join(root, "token"), "utf8")).trim();
  const client = await RpcClient.connect(process.env["ANAMNESIS_RUNTIME_SOCKET"] ?? socketPath(root), token);
  try {
    if (command === "ingest") { await ingestSource(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-agentlog") { await ingestAgentLog(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-slack") { await ingestSlack(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-notion") { await ingestNotion(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-claude-raw") { await ingestClaudeRaw(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-codex-raw") { await ingestCodexRaw(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-gjc-raw") { await ingestGjcRaw(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-omo-raw") { await ingestOmoRaw(extra[0]!, extra[1]!, client); return; }
    if (command === "ingest-misc-raw") { await ingestMiscRaw(extra[0]!, extra[1]!, client); return; }
    const status = await client.request("status", {});
    if (command === "status") { console.log(JSON.stringify(status)); return; }
    const checks = {
      root_private: ((await lstat(root)).mode & 0o777) === 0o700,
      token_private: ((await lstat(join(root, "token"))).mode & 0o777) === 0o600,
      socket_private: ((await lstat(socketPath(root))).mode & 0o777) === 0o600,
      authenticated: true,
      database_writer_fence: status.capabilities.writer_fence === "database",
      storage_available: status.storage === "available",
      spool_clean: status.spool.quarantined === 0 && status.spool.blocked === 0,
    };
    const ok = Object.values(checks).every(Boolean);
    console.log(JSON.stringify({ ok, scope: "runtime-admission-health; not a full database integrity audit", checks, status }));
    if (!ok) process.exitCode = 1;
  } finally { await client.close(); }
}
main().catch(error => {
  console.error(JSON.stringify({ event: "operation_failed", code: error.code ?? "operation_failed", error: String(error) }));
  process.exitCode = 1;
});
