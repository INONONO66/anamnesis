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
const usage = "usage: anamnesis-ops up|down|backup <destination-dir>|restore <archive-dir>|extract|embed|embed-requeue [--limit N]|recall <query>|foreground|managed|status|verify|ingest <snapshot.jsonl> <checkpoint.json>";
const uuidv7 = () => { const value = randomUUID(); return `${value.slice(0, 14)}7${value.slice(15, 19)}8${value.slice(20)}`; };
const fail = (code: string, exit = 1): never => { console.log(JSON.stringify({ error: code })); process.exit(exit); };

/** Remote client mode: ANAMNESIS_RPC_TCP="host:port" targets a daemon's TCP listener
 * (docs/deploy.md, "Optional TCP listener"). The listener bearer is read from
 * ANAMNESIS_RPC_TCP_TOKEN_FILE and the installation token from ANAMNESIS_RUNTIME_TOKEN_FILE,
 * both regular 0600 files owned by the caller (same rule as ANAMNESIS_LISTEN_TOKEN_FILE).
 * Unset = local socket under the runtime root, unchanged. */
async function readTokenFile(variable: string): Promise<string> {
  const file = process.env[variable];
  if (!file) throw new Error(`${variable} is required with ANAMNESIS_RPC_TCP`);
  const info = await lstat(file).catch(() => { throw new Error(`unable to read ${variable}`); });
  if (!info.isFile() || info.isSymbolicLink() || info.uid !== process.getuid?.() || (info.mode & 0o777) !== 0o600) throw new Error(`${variable} must be a regular file owned by the current user with mode 0600`);
  const token = (await readFile(file, "utf8")).trim();
  if (!token || /[\r\n]/.test(token) || Buffer.byteLength(token) > 1024) throw new Error(`${variable} must hold one token of at most 1024 bytes`);
  return token;
}
async function connectClient(root: string): Promise<RpcClient> {
  const remote = process.env["ANAMNESIS_RPC_TCP"];
  if (remote) {
    const at = remote.lastIndexOf(":"), host = remote.slice(0, at).replace(/^\[|\]$/g, ""), port = Number(remote.slice(at + 1));
    if (at < 1 || !host || !Number.isInteger(port) || port < 1 || port > 65535) throw new Error("ANAMNESIS_RPC_TCP must be host:port");
    const [bearer, installation] = await Promise.all([readTokenFile("ANAMNESIS_RPC_TCP_TOKEN_FILE"), readTokenFile("ANAMNESIS_RUNTIME_TOKEN_FILE")]);
    return RpcClient.connect({ host, port, token: bearer }, installation);
  }
  const token = (await readFile(join(root, "token"), "utf8")).trim();
  return RpcClient.connect(process.env["ANAMNESIS_RUNTIME_SOCKET"] ?? socketPath(root), token);
}
interface Context { root: string; socket: string; extra: string[] }
interface Command { arity(extra: string[]): boolean; run(context: Context): Promise<void> }
const errorCode = (error: unknown): string => error && typeof error === "object" && "code" in error ? String(error.code) : "unknown";
const offline = (error: unknown): boolean => { const code = errorCode(error); return code === "ENOENT" || code === "ECONNREFUSED"; };
const print = (value: unknown): void => console.log(JSON.stringify(value));
const none = (extra: string[]) => extra.length === 0;
const one = (extra: string[]) => extra.length === 1;
const ingestHandlers: Record<string, (a: string, b: string, c: RpcClient) => Promise<void>> = { ingest: ingestSource, "ingest-agentlog": ingestAgentLog, "ingest-slack": ingestSlack, "ingest-notion": ingestNotion, "ingest-claude-raw": ingestClaudeRaw, "ingest-codex-raw": ingestCodexRaw, "ingest-gjc-raw": ingestGjcRaw, "ingest-omo-raw": ingestOmoRaw, "ingest-misc-raw": ingestMiscRaw };

async function withClient<T>(root: string, action: (client: RpcClient) => Promise<T>): Promise<T> {
  const client = await connectClient(root);
  try { return await action(client); } finally { await client.close(); }
}
/** backup/restore never run beside a live daemon owning the root. */
async function assertNoLiveOwner(root: string): Promise<void> {
  try { const owner = JSON.parse(await readFile(join(root, "owner", "owner.json"), "utf8")); if (owner.pid === process.pid || (Number.isSafeInteger(owner.pid) && (() => { try { process.kill(owner.pid, 0); return true; } catch { return false; } })())) fail("daemon_live"); } catch (error) { const code = errorCode(error); if (code !== "ENOENT") throw error; }
}
async function up({ socket }: Context): Promise<void> {
  let config;
  try { config = await loadProviderConfig(process.env); } catch { fail("extraction_provider_required", 2); }
  if (!config || !config.llm.baseUrl) fail("extraction_provider_required", 2);
  const child = spawn(process.execPath, [fileURLToPath(import.meta.url), "managed"], { env: process.env, stdio: "ignore", detached: true });
  child.unref();
  const deadline = Date.now() + 90000;
  while (Date.now() < deadline) { try { await lstat(socket); return; } catch (error) { const code = errorCode(error); if (code !== "ENOENT") throw error; await new Promise(resolve => setImmediate(resolve)); } }
  fail("runtime_start_timeout");
}
async function down({ root }: Context): Promise<void> {
  try { await withClient(root, client => client.request("shutdown", {})); }
  catch (error) { if (!offline(error)) throw error; }
}
async function backup({ root, extra }: Context): Promise<void> {
  await assertNoLiveOwner(root);
  let client: RpcClient;
  try { client = await connectClient(root); }
  catch (error) { if (!offline(error)) throw error; const result = await offlineBackup(root, extra[0]!); console.log(JSON.stringify({ state: "complete", operation_id: result.operation_id, manifest: { format: result.manifest.format, members: result.manifest.members.length, objects: result.manifest.objects.length } })); return; }
  try { print(await client.request("backup", { operation_id: uuidv7(), destination: extra[0]! })); } finally { await client.close(); }
}
async function restore({ root, socket, extra }: Context): Promise<void> {
  await assertNoLiveOwner(root);
  try { await lstat(socket); } catch (error) { const code = errorCode(error); if (code === "ENOENT" || code === "ECONNREFUSED") { const result = await offlineRestore(root, extra[0]!); console.log(JSON.stringify({ state: "complete", operation_id: result.operation_id, manifest: { format: result.manifest.format, members: result.manifest.members.length }, container: result.container, uri: result.uri })); return; } throw error; }
  await withClient(root, async client => print(await client.request("restore", { operation_id: uuidv7(), archive: extra[0]! })));
}
async function embed({ root }: Context): Promise<void> {
  let client: RpcClient;
  try { client = await connectClient(root); }
  catch { print({ embeddings: "disabled", drained: 0 }); return; }
  try {
    const status = await client.request("status", {});
    print(status.capabilities.embeddings ? { embeddings: "enabled", drained: 0 } : { embeddings: "disabled", drained: 0 });
  } finally { await client.close(); }
}
/** Quarantined Episodes return to the embedding outbox; the daemon wakes its embedding lane on the call. */
async function embedRequeue({ root, extra }: Context): Promise<void> {
  const limit = extra.length ? Number(extra[1]) : 100;
  await withClient(root, async client => print({ ...await client.request("embedding.requeue", { limit }), limit }));
}
async function extract({ root }: Context): Promise<void> {
  await withClient(root, async client => { const status = await client.request("status", {}); if (!status.capabilities.extraction) fail("extraction_provider_required", 2); console.log(JSON.stringify({ extraction: "ready" })); });
}
async function verify({ root, socket }: Context): Promise<void> {
  await withClient(root, async client => {
    const status = await client.request("status", {});
    // The three filesystem checks describe the daemon's own root; over ANAMNESIS_RPC_TCP the caller has no view of it.
    const remote = process.env["ANAMNESIS_RPC_TCP"] !== undefined;
    const checks = remote ? { authenticated: true, database_writer_fence: status.capabilities.writer_fence === "database", storage_available: status.storage === "available", spool_clean: status.spool.quarantined === 0 && status.spool.blocked === 0 } : { root_private: ((await lstat(root)).mode & 0o777) === 0o700, token_private: ((await lstat(join(root, "token"))).mode & 0o777) === 0o600, socket_private: ((await lstat(socket)).mode & 0o777) === 0o600, authenticated: true, database_writer_fence: status.capabilities.writer_fence === "database", storage_available: status.storage === "available", spool_clean: status.spool.quarantined === 0 && status.spool.blocked === 0 };
    const ok = Object.values(checks).every(Boolean); console.log(JSON.stringify({ ok, scope: remote ? "remote runtime-admission-health (filesystem checks skipped over TCP); not a full database integrity audit" : "runtime-admission-health; not a full database integrity audit", checks, status })); if (!ok) process.exitCode = 1;
  });
}
const ingest = (command: string): Command => ({ arity: extra => extra.length === 2, run: ({ root, extra }) => withClient(root, client => ingestHandlers[command]!(extra[0]!, extra[1]!, client)) });
const commands: Record<string, Command> = {
  up: { arity: none, run: up },
  down: { arity: none, run: down },
  backup: { arity: one, run: backup },
  restore: { arity: one, run: restore },
  extract: { arity: none, run: extract },
  embed: { arity: none, run: embed },
  "embed-requeue": { arity: extra => extra.length === 0 || (extra.length === 2 && extra[0] === "--limit" && Number.isInteger(Number(extra[1])) && Number(extra[1]) >= 1 && Number(extra[1]) <= 1000), run: embedRequeue },
  recall: { arity: one, run: ({ root, extra }) => withClient(root, async client => print(await client.request("recall", { query: extra[0]!, limit: 10 }))) },
  status: { arity: none, run: ({ root }) => withClient(root, async client => print(await client.request("status", {}))) },
  verify: { arity: none, run: verify },
  ...Object.fromEntries(ingestCommands.map(command => [command, ingest(command)])),
};

async function main(): Promise<void> {
  const [command = "status", ...extra] = process.argv.slice(2);
  if (command === "foreground" && !extra.length) { await foreground(); return; }
  if (command === "managed" && !extra.length) { await managed(fileURLToPath(import.meta.url)); return; }
  const entry = commands[command];
  if (!entry || !entry.arity(extra)) throw new Error(usage);
  const root = runtimeRoot(), socket = process.env["ANAMNESIS_RUNTIME_SOCKET"] ?? socketPath(root);
  await applyAuthorityEnvironment(root);
  await entry.run({ root, socket, extra });
}
main().catch(error => { console.error(JSON.stringify({ event: "operation_failed", code: errorCode(error), error: String(error) })); process.exitCode = 1; });
