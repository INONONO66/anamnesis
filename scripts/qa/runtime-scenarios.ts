import { randomBytes } from "node:crypto";
import { mkdir, writeFile } from "node:fs/promises";
import { spawn } from "node:child_process";

const args = process.argv.slice(2);
const caseName = args[args.indexOf("--case") + 1];
const evidenceRoot = args[args.indexOf("--evidence-root") + 1];
if (caseName !== "foundation") throw new Error(`Unknown scenario case: ${caseName ?? "missing"}`);
if (!evidenceRoot) throw new Error("--evidence-root is required");
const name = `anamnesis-qa-${process.pid}-${Date.now()}`;
const password = `qa-${randomBytes(18).toString("base64url")}`;
const run = (command: string, commandArgs: string[], options: Parameters<typeof spawn>[2] = {}) => new Promise<{ code: number; output: string }>((resolve, reject) => {
  const child = spawn(command, commandArgs, { ...options, stdio: ["ignore", "pipe", "pipe"] }); let output = "";
  child.stdout.on("data", (data: Buffer) => { output += data.toString(); }); child.stderr.on("data", (data: Buffer) => { output += data.toString(); });
  child.on("error", reject); child.on("close", (code) => resolve({ code: code ?? 1, output }));
});
await mkdir(evidenceRoot, { recursive: true });
let containerId: string | undefined; let childResult: { code: number; output: string } | undefined; let cleanupResult = "not attempted";
try {
  const created = await run("docker", ["create", "--name", name, "-p", "127.0.0.1::7687", "-e", `NEO4J_AUTH=neo4j/${password}`, "neo4j:5.26-community"]);
  if (created.code !== 0) throw new Error(created.output); containerId = created.output.trim().split("\n").at(-1); if (!containerId) throw new Error("docker create returned no id");
  const attached = spawn("docker", ["start", "-a", name], { stdio: ["ignore", "pipe", "pipe"] }); let readiness = "";
  const ready = new Promise<void>((resolve, reject) => { const timer = setTimeout(() => reject(new Error("Neo4j Started event timed out")), 180_000); const onData = (data: Buffer) => { readiness += data.toString(); if (/Started\.?/.test(readiness)) { clearTimeout(timer); resolve(); } }; attached.stdout.on("data", onData); attached.stderr.on("data", onData); });
  await ready;
  const port = await run("docker", ["inspect", "-f", "{{(index (index .NetworkSettings.Ports \"7687/tcp\") 0).HostPort}}", name]); const hostPort = port.output.trim(); if (!/^\d+$/.test(hostPort)) throw new Error(`Unable to inspect mapped port: ${port.output}`);
  const uri = `bolt://127.0.0.1:${hostPort}`;
  childResult = await run("bun", ["test", "packages"], { env: { ...process.env, ANAMNESIS_TEST_NEO4J_URI: uri, ANAMNESIS_TEST_NEO4J_USER: "neo4j", ANAMNESIS_TEST_NEO4J_PASSWORD: password, ANAMNESIS_NEO4J_PASSWORD: password } });
  await writeFile(`${evidenceRoot}/child-output.txt`, childResult.output + readiness); if (childResult.code !== 0) throw new Error(`Test suite exited ${childResult.code}`);
} finally { if (containerId) { const removed = await run("docker", ["rm", "-f", name]); cleanupResult = `${removed.output}exit=${removed.code}`; } await writeFile(`${evidenceRoot}/cleanup.txt`, `container=${name}\n${cleanupResult}\nchildExit=${childResult?.code ?? "not completed"}\n`); }
