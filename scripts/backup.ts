import { createWriteStream } from "node:fs";
import { mkdir, rename, rm } from "node:fs/promises";
import { join } from "node:path";
import { spawn } from "node:child_process";
import { pipeline } from "node:stream/promises";

const PINNED_IMAGE = "neo4j:5.26-community";

function containerArgument(args: string[]): string {
  if (args.length === 0) {
    const configured = process.env["NEO4J_CONTAINER"];
    if (!configured) {
      throw new Error("Pass --container <name> or set NEO4J_CONTAINER");
    }
    return configured;
  }
  if (args.length !== 2 || args[0] !== "--container" || !args[1]) {
    throw new Error("Usage: bun run backup -- [--container <name>]");
  }
  return args[1];
}

async function run(command: string, args: string[]): Promise<void> {
  const child = spawn(command, args, { stdio: "inherit" });
  const code = await new Promise<number>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (status) => resolve(status ?? 1));
  });
  if (code !== 0) throw new Error(`${command} exited with status ${code}`);
}

async function dump(container: string, destination: string): Promise<void> {
  const child = spawn(
    "docker",
    [
      "run",
      "--rm",
      "--volumes-from",
      container,
      PINNED_IMAGE,
      "neo4j-admin",
      "database",
      "dump",
      "neo4j",
      "--to-stdout",
    ],
    { stdio: ["ignore", "pipe", "inherit"] },
  );
  if (!child.stdout) throw new Error("Docker dump stdout was not available");
  const completion = new Promise<number>((resolve, reject) => {
    child.once("error", reject);
    child.once("close", (status) => resolve(status ?? 1));
  });
  const [, code] = await Promise.all([
    pipeline(child.stdout, createWriteStream(destination, { flags: "wx" })),
    completion,
  ]);
  if (code !== 0) throw new Error(`Neo4j dump exited with status ${code}`);
}

const container = containerArgument(process.argv.slice(2));
const directory = join(process.cwd(), "backups");
const destination = join(directory, `neo4j-${new Date().toISOString()}.dump`);
const partial = `${destination}.partial`;
await mkdir(directory, { recursive: true });

console.log(`Stopping ${container} for a consistent Community Edition offline dump...`);
await run("docker", ["stop", container]);
let completed = false;
try {
  console.log(`Dumping neo4j from ${container} through ${PINNED_IMAGE}...`);
  await dump(container, partial);
  await rename(partial, destination);
  completed = true;
  console.log(`Backup written to ${destination}`);
} finally {
  if (!completed) await rm(partial, { force: true });
  console.log(`Restarting ${container}...`);
  await run("docker", ["start", container]);
}
