import { randomUUID, createHash } from "node:crypto";
import { cp, mkdir, readFile, writeFile, lstat } from "node:fs/promises";
import { spawn } from "node:child_process";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { Engine } from "../../packages/core/src/engine.ts";
import { acquireInstallation } from "./config.ts";
import { backupOwned, restoreOwned, type TrustedAuthorityAdapter } from "./backup-restore-orchestrator.ts";
import { preflightArchive, type ArchiveCompatibility, type ArchiveManifest } from "./archive-manifest.ts";
import { createRuntimeAuthority, manifestTemplate, objectInventory } from "./runtime-authority.ts";
import { OwnedNeo4jAdapter, NEO4J_IMAGE, NEO4J_VERSION } from "./owned-neo4j-adapter.ts";
import type { InstallationContext } from "../../packages/core/src/store.ts";

const OWNER_LABEL = "anamnesis.qa.owner";
const IMAGE_DIGEST = NEO4J_IMAGE.slice("neo4j@".length);
const compatibility: ArchiveCompatibility = { schema_versions: ["anamnesis.storage/1"], neo4j_versions: [NEO4J_VERSION], neo4j_image_digests: [IMAGE_DIGEST], episode_digest_version_ceiling: 2 };
const context = (): InstallationContext => ({ principal: "installation", commit_mode: "auto", client_binding: randomUUID() });
const configBytes = () => Buffer.from(JSON.stringify({ uri: process.env["ANAMNESIS_NEO4J_URI"] ?? "", user: process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j", database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j" }));
const hash = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");

async function execDocker(args: string[]): Promise<string> {
  const child = spawn("docker", args, { stdio: ["ignore", "pipe", "pipe"] });
  let out = "", err = ""; child.stdout?.on("data", b => out += b); child.stderr?.on("data", b => err += b);
  const code = await new Promise<number>((resolve, reject) => { child.once("error", reject); child.once("close", c => resolve(c ?? 1)); });
  if (code !== 0) throw new Error(`docker failed (${code}): ${err.slice(-1000)}`);
  return out.trim();
}
async function waitBolt(uri: string, password: string): Promise<void> {
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
  try { const deadline = Date.now() + 90000; while (true) { try { await driver.verifyConnectivity(); return; } catch (error) { if (Date.now() >= deadline) throw error; } } }
  finally { await driver.close(); }
}
async function uniqueOperation(): Promise<string> { const value = randomUUID(); return `${value.slice(0, 14)}7${value.slice(15, 19)}8${value.slice(20)}`; }

export async function applyAuthorityEnvironment(root: string): Promise<void> {
  try {
    const authorityPath = join(root, "authority.json"), info = await lstat(authorityPath);
    if (!info.isFile() || info.isSymbolicLink() || info.uid !== process.getuid?.() || (info.mode & 0o077) !== 0) throw Object.assign(new Error("unsafe_authority"), { code: "unsafe_path" });
    const raw = JSON.parse(await readFile(authorityPath, "utf8")) as { uri?: string; database?: string; container?: string; owner?: string };
    if (process.env["ANAMNESIS_NEO4J_URI"] === undefined && raw.uri) process.env["ANAMNESIS_NEO4J_URI"] = raw.uri;
    if (process.env["ANAMNESIS_NEO4J_DATABASE"] === undefined && raw.database) process.env["ANAMNESIS_NEO4J_DATABASE"] = raw.database;
    if (process.env["ANAMNESIS_NEO4J_CONTAINER"] === undefined && raw.container) process.env["ANAMNESIS_NEO4J_CONTAINER"] = raw.container;
    if (process.env["ANAMNESIS_QA_OWNER"] === undefined && raw.owner) process.env["ANAMNESIS_QA_OWNER"] = raw.owner;
  } catch (error) { if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error; }
}

function makeManifest(operationId: string, cutoff: ArchiveManifest["cutoff"], authority: ArchiveManifest["authority"], objects: ArchiveManifest["objects"]): ArchiveManifest {
  const bytes = configBytes(); return manifestTemplate(operationId, cutoff, authority!, objects, hash(bytes));
}
export async function offlineBackup(root: string, destination: string): Promise<{ operation_id: string; manifest: ArchiveManifest }> {
  const installation = await acquireInstallation(root);
  const engine = new Engine();
  const operationId = await uniqueOperation();
  try {
    await engine.init(); await engine.claimWriterEpoch();
    const adapter = await createRuntimeAuthority(engine, installation, context());
    const fenced = await adapter.revokeWriters();
    const authority = await adapter.authoritySnapshot(fenced.epoch);
    const objects = await objectInventory(join(root, "objects"));
    const manifest = makeManifest(operationId, fenced.cutoff, authority, objects);
    const cached: TrustedAuthorityAdapter = {
      revokeWriters: async () => fenced, authoritySnapshot: async () => authority,
      dumpOffline: adapter.dumpOffline.bind(adapter), materializeMembers: adapter.materializeMembers.bind(adapter), startAndReady: adapter.startAndReady.bind(adapter), stop: adapter.stop.bind(adapter), restoreOffline: adapter.restoreOffline.bind(adapter), rebindSource: adapter.rebindSource.bind(adapter), verifyPhysicalLinks: adapter.verifyPhysicalLinks.bind(adapter), quarantine: adapter.quarantine.bind(adapter),
    };
    await backupOwned({ root, destination, operationId, compatibility, manifest, objectRoot: join(root, "objects") }, cached);
    return { operation_id: operationId, manifest };
  } finally { await engine.close(); await installation.release(); }
}

export async function offlineRestore(root: string, archive: string): Promise<{ operation_id: string; manifest: ArchiveManifest; container: string; uri: string }> {
  const admitted = await preflightArchive(archive, compatibility);
  const operationId = admitted.manifest.operation_id;
  const token = (() => { try { return process.env["ANAMNESIS_RUNTIME_TOKEN"] ?? randomUUID(); } catch { return randomUUID(); } })();
  const incarnation = randomUUID();
  const staging = `${root}.restore-staging.${operationId}`, rollback = `${root}.restore-rollback.${operationId}`;
  const owner = process.env["ANAMNESIS_QA_OWNER"];
  if (!owner) throw Object.assign(new Error("owner_required"), { code: "owner_required" });
  let container = "", uri = "";
  const password = process.env["ANAMNESIS_NEO4J_PASSWORD"] ?? "g3-isolated-password";
  const authority = {
    revokeWriters: async () => ({ epoch: String(admitted.manifest.cutoff.ingest_seq), cutoff: admitted.manifest.cutoff }),
    authoritySnapshot: async () => admitted.manifest.authority!,
    materializeMembers: async () => {},
    startAndReady: async (live: string, epoch: string) => {
      container = await execDocker(["run", "-d", "--label", `${OWNER_LABEL}=${owner}`, "-p", "127.0.0.1::7687", "-v", `${live}/database:/data`, "-e", `NEO4J_AUTH=neo4j/${password}`, NEO4J_IMAGE]);
      const portText = await execDocker(["port", container, "7687/tcp"]);
      const port = Number(portText.split(":").at(-1)); uri = `bolt://127.0.0.1:${port}`;
      await waitBolt(uri, password);
      await writeFile(join(live, "authority.json"), JSON.stringify({ container, uri, database: "neo4j", owner }) + "\n", { mode: 0o600 });
      return { sourceId: incarnation, epoch, ready: true };
    },
    rebindSource: async () => {}, verifyPhysicalLinks: async () => {}, quarantine: async () => {},
  };
  const adapter = new OwnedNeo4jAdapter({ container: "restore-placeholder", owner, authority, lifecycle: { stop: async () => {} } });
  const originalExists = await lstat(root).then(() => true).catch(() => false);
  if (!originalExists) await mkdir(root, { recursive: true, mode: 0o700 });
  // The activation moves the complete root. Seed the new root's installation
  // identity after the dump is loaded, while preserving the owner-only mode.
  const originalRestore = adapter.restoreOffline.bind(adapter);
  adapter.restoreOffline = async (inputArchive, inputStaging, manifest) => {
    await originalRestore(inputArchive, inputStaging, manifest);
    await writeFile(join(inputStaging, "token"), token + "\n", { mode: 0o600 });
    await writeFile(join(inputStaging, "incarnation.json"), JSON.stringify(incarnation) + "\n", { mode: 0o600 });
    await mkdir(join(inputStaging, "objects"), { recursive: true, mode: 0o700 });
    await mkdir(join(inputStaging, "uploads"), { recursive: true, mode: 0o700 });
    await mkdir(join(inputStaging, "spool"), { recursive: true, mode: 0o700 });
    await mkdir(join(inputStaging, "deliveries"), { recursive: true, mode: 0o700 });
    try { await cp(join(inputArchive, "objects"), join(inputStaging, "objects"), { recursive: true }); } catch (error) { if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error; }
  };
  // restoreOwned requires an owned parent and a live directory. The existing
  // empty target root is the live reservation; no daemon owner is held yet.
  await restoreOwned({ archive, liveRoot: root, stagingRoot: staging, rollbackRoot: rollback, operationId, compatibility, expectedSourceId: incarnation }, adapter);
  return { operation_id: operationId, manifest: admitted.manifest, container, uri };
}
