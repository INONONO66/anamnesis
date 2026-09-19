import { createReadStream } from "node:fs";
import { spawn } from "node:child_process";
import { mkdir, open, stat } from "node:fs/promises";
import { pipeline } from "node:stream/promises";
import { join } from "node:path";
import type { ArchiveManifest } from "./archive-manifest.ts";
import type { TrustedAuthorityAdapter } from "./backup-restore-orchestrator.ts";

/** Community image used by the authority adapter. A tag is intentionally not accepted. */
export const NEO4J_IMAGE = "neo4j@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d";
export const NEO4J_VERSION = "5.26.30";
const OWNER_LABEL = "anamnesis.qa.owner";

type Exec = (args: string[], options?: { stdin?: Uint8Array }) => Promise<{ stdout: string }>;
export interface OwnedNeo4jAdapterOptions {
  container: string;
  owner: string;
  exec?: Exec;
  authority: Pick<TrustedAuthorityAdapter, "revokeWriters" | "authoritySnapshot" | "materializeMembers" | "startAndReady" | "rebindSource" | "verifyPhysicalLinks" | "quarantine">;
  lifecycle: Pick<TrustedAuthorityAdapter, "stop">;
}

async function command(args: string[]): Promise<{ stdout: string }> {
  if (args[0] !== "docker") throw new Error("owned adapter only permits docker");
  const child = spawn("docker", args.slice(1), { stdio: ["ignore", "pipe", "pipe"] });
  let stdout = Buffer.alloc(0), stderr = "";
  child.stdout?.on("data", (b: Buffer) => { stdout = Buffer.concat([stdout, b]); });
  child.stderr?.on("data", (b: Buffer) => { stderr += b.toString(); });
  const code = await new Promise<number>((resolve, reject) => { child.once("error", reject); child.once("close", c => resolve(c ?? 1)); });
  if (code !== 0) throw new Error(`docker failed (${code}): ${stderr.slice(-1000)}`);
  return { stdout: stdout.toString() };
}

async function fsync(path: string): Promise<void> { const file = await open(path, "r"); try { await file.sync(); } finally { await file.close(); } }

/** Trusted adapter: validates ownership before every offline operation and never uses a mutable image tag. */
export class OwnedNeo4jAdapter implements TrustedAuthorityAdapter {
  private readonly run: Exec;
  constructor(private readonly options: OwnedNeo4jAdapterOptions) { this.run = options.exec ?? command; }

  private async assertOwned(): Promise<void> {
    const result = await this.run(["docker", "inspect", "--format", "{{json .}}", this.options.container]);
    const value = JSON.parse(result.stdout) as { Config?: { Labels?: Record<string, string> }; State?: { Running?: boolean } };
    if (value.Config?.Labels?.[OWNER_LABEL] !== this.options.owner) throw new Error("owned_container_required");
  }

  revokeWriters() { return this.options.authority.revokeWriters(); }
  authoritySnapshot(epoch: string, limit?: number) { return this.options.authority.authoritySnapshot(epoch, limit); }
  materializeMembers(root: string, manifest: ArchiveManifest) { return this.options.authority.materializeMembers(root, manifest); }
  startAndReady(root: string, epoch: string) { return this.options.authority.startAndReady(root, epoch); }
  rebindSource(sourceId: string) { return this.options.authority.rebindSource(sourceId); }
  verifyPhysicalLinks(root: string) { return this.options.authority.verifyPhysicalLinks(root); }
  quarantine(root: string) { return this.options.authority.quarantine(root); }
  stop() { return this.options.lifecycle.stop(); }

  async dumpOffline(destination: string, epoch: string) {
    await this.assertOwned();
    if (!/^\d+$/.test(epoch)) throw new Error("invalid_writer_epoch");
    const child = spawn("docker", ["run", "--rm", "--volumes-from", this.options.container, NEO4J_IMAGE, "neo4j-admin", "database", "dump", "neo4j", "--to-stdout"], { stdio: ["ignore", "pipe", "pipe"] });
    if (!child.stdout) throw new Error("dump_stdout_unavailable");
    let stderr = ""; child.stderr?.on("data", b => { stderr += b.toString(); });
    const completion = new Promise<number>((resolve, reject) => { child.once("error", reject); child.once("close", c => resolve(c ?? 1)); });
    await pipeline(child.stdout, (await import("node:fs")).createWriteStream(destination, { flags: "wx", mode: 0o600 }));
    if (await completion !== 0) throw new Error(`neo4j_dump_failed: ${stderr.slice(-1000)}`);
    await fsync(destination);
    const bytes = (await stat(destination)).size;
    if (bytes <= 0) throw new Error("empty_neo4j_dump");
    return { metadata: Buffer.from(JSON.stringify({ format: "anamnesis.adapter-dump/1", epoch, bytes }) + "\n"), neo4jVersion: NEO4J_VERSION, imageDigest: NEO4J_IMAGE.slice("neo4j@".length) };
  }

  async restoreOffline(archive: string, staging: string, _manifest: ArchiveManifest): Promise<void> {
    await mkdir(join(staging, "database"), { recursive: true, mode: 0o700 });
    const dump = join(archive, "database", "neo4j.dump");
    const child = spawn("docker", ["run", "--rm", "--user", "0:0", "--entrypoint", "neo4j-admin", "-i", "-v", `${staging}/database:/data`, NEO4J_IMAGE, "database", "load", "neo4j", "--from-stdin", "--overwrite-destination=true"], { stdio: ["pipe", "ignore", "pipe"] });
    let stderr = ""; child.stderr?.on("data", b => { stderr += b.toString(); });
    const completion = new Promise<number>((resolve, reject) => { child.once("error", reject); child.once("close", c => resolve(c ?? 1)); });
    await pipeline(createReadStream(dump, { flags: "r" }), child.stdin!);
    if (await completion !== 0) throw new Error(`neo4j_load_failed: ${stderr.slice(-1000)}`);
  }
}
