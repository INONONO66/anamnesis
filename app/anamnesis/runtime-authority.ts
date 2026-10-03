import { createHash } from "node:crypto";
import { promisify } from "node:util";
import { execFile } from "node:child_process";
import { readdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import { OwnedNeo4jAdapter, NEO4J_IMAGE, NEO4J_VERSION } from "./owned-neo4j-adapter.ts";
import type { TrustedAuthorityAdapter } from "./backup-restore-orchestrator.ts";
import type { ArchiveManifest, AuthoritySnapshot } from "./archive-manifest.ts";
import { Store, type StoreOptions, type InstallationContext } from "@anamnesis/core";
import type { Engine } from "@anamnesis/core";
import type { Installation } from "./config.ts";

const OWNER_LABEL = "anamnesis.qa.owner";
const sha256 = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
const dockerExec = promisify(execFile);
const dockerOutput = async (args: string[]) => (await dockerExec("docker", args)).stdout.trim();
const neo4jUser = () => process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j";
const neo4jPassword = () => process.env["ANAMNESIS_NEO4J_PASSWORD"] ?? "";

/** Waits for Bolt to answer while the database boots; a security refusal cannot heal with time and is thrown at once. */
async function waitBolt(uri: string, user: string, password: string): Promise<void> {
  const driver = neo4j.driver(uri, neo4j.auth.basic(user, password), { connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
  try {
    const deadline = Date.now() + 90000;
    while (true) {
      try { await driver.verifyConnectivity(); return; }
      catch (error) {
        if (error instanceof neo4j.Neo4jError && error.code.startsWith("Neo.ClientError.Security.")) throw error;
        if (Date.now() >= deadline) throw error;
      }
    }
  } finally { await driver.close(); }
}

export interface RestoredDatabase { container: string; uri: string; user: "neo4j"; database: "neo4j"; password: string }

/** Removes a container this process started. Only confirmed absence is tolerated; any other docker failure propagates. */
export const removeContainer = async (container: string) => {
  try { await dockerExec("docker", ["rm", "-f", container]); }
  catch (error) {
    if (!/no such (object|container)/i.test(String((error as { stderr?: unknown }).stderr ?? error))) throw error;
  }
};

/** Starts an owner-labelled Neo4j on the database `restoreOffline` loaded under `root`, waits for Bolt,
 * and records the endpoint in `<root>/authority.json` so a later `ops up` binds to the restored database.
 * The fresh container always serves user `neo4j` and database `neo4j`, whatever the source was configured with;
 * a container that never became ready is removed before the failure propagates. */
export async function startRestoredDatabase(root: string, owner: string, password: string): Promise<RestoredDatabase> {
  const container = await dockerOutput(["run", "-d", "--label", `${OWNER_LABEL}=${owner}`, "-p", "127.0.0.1::7687", "-v", `${join(root, "database")}:/data`, "-e", `NEO4J_AUTH=neo4j/${password}`, NEO4J_IMAGE]);
  try {
    const mapped = await dockerOutput(["port", container, "7687/tcp"]);
    const uri = `bolt://127.0.0.1:${Number(mapped.split(":").at(-1))}`;
    await waitBolt(uri, "neo4j", password);
    await writeFile(join(root, "authority.json"), JSON.stringify({ container, uri, database: "neo4j", owner }) + "\n", { mode: 0o600 });
    return { container, uri, user: "neo4j", database: "neo4j", password };
  } catch (error) {
    // The start failure is the error to report; a removal failure is appended, never substituted.
    try { await removeContainer(container); }
    catch (cleanup) { throw Object.assign(new Error(`${String(error)}; container ${container} not removed: ${String(cleanup)}`), { code: "restored_container_leaked", cause: error }); }
    throw error;
  }
}

/** The orchestrator fences again; serve the fence and snapshot already taken so the stopped source is never asked twice. */
export function fencedAdapter(adapter: TrustedAuthorityAdapter, fenced: Awaited<ReturnType<TrustedAuthorityAdapter["revokeWriters"]>>, authority: AuthoritySnapshot): TrustedAuthorityAdapter {
  return {
    revokeWriters: async () => fenced,
    authoritySnapshot: async () => authority,
    restoredAuthoritySnapshot: adapter.restoredAuthoritySnapshot.bind(adapter),
    dumpOffline: adapter.dumpOffline.bind(adapter),
    materializeMembers: adapter.materializeMembers.bind(adapter),
    startAndReady: adapter.startAndReady.bind(adapter),
    startRestored: adapter.startRestored.bind(adapter),
    stop: adapter.stop.bind(adapter),
    restoreOffline: adapter.restoreOffline.bind(adapter),
    rebindSource: adapter.rebindSource.bind(adapter),
    verifyPhysicalLinks: adapter.verifyPhysicalLinks.bind(adapter),
    quarantine: adapter.quarantine.bind(adapter),
  };
}

/** Read restored authority without init/migrations rewriting the restored graph. */
export async function readRestoredAuthority(options: StoreOptions, context: InstallationContext): Promise<AuthoritySnapshot> {
  const store = new Store(options);
  try {
    await store.claimWriterEpoch();
    return await store.authoritySnapshot(context);
  } finally { await store.close(); }
}

/** The lifecycle-owned adapter used by the runtime. It is intentionally built
 * per authenticated operation so the Store authority context cannot be lost. */
export async function createRuntimeAuthority(engine: Engine, installation: Installation, context: InstallationContext): Promise<TrustedAuthorityAdapter> {
  const container = process.env["ANAMNESIS_NEO4J_CONTAINER"];
  const owner = process.env["ANAMNESIS_QA_OWNER"];
  if (!container || !owner) throw Object.assign(new Error("backup_adapter_unavailable"), { code: "backup_adapter_unavailable" });
  let cutoff: ArchiveManifest["cutoff"] | undefined;
  let authorityEvidence: AuthoritySnapshot | undefined;
  let restored: RestoredDatabase | undefined;
  const authority = {
    revokeWriters: async () => {
      const epoch = String(await engine.claimWriterEpoch());
      const snapshot = await engine.store.authoritySnapshot(context);
      cutoff = snapshot.coverage;
      authorityEvidence = snapshot;
      const running = await dockerOutput(["inspect", "--format", "{{.State.Running}}", container]);
      if (running === "true") await dockerOutput(["stop", container]);
      return { epoch, cutoff };
    },
    authoritySnapshot: async (_epoch: string) => {
      if (!cutoff || !authorityEvidence) throw new Error("writer_fence_required");
      return authorityEvidence;
    },
    restoredAuthoritySnapshot: async () => {
      if (!restored) throw new Error("restore_not_started");
      return readRestoredAuthority({ uri: restored.uri, user: restored.user, password: restored.password, database: restored.database }, context);
    },
    materializeMembers: async (root: string, manifest: ArchiveManifest) => {
      const config = Buffer.from(JSON.stringify({ uri: process.env["ANAMNESIS_NEO4J_URI"] ?? "", user: process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j", database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j" }));
      await writeFile(join(root, "config.jsonc"), config, { flag: "wx", mode: 0o600 });
      await writeFile(join(root, "neo4j.auth"), Buffer.from(JSON.stringify({ database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j" })), { flag: "wx", mode: 0o600 });
      const configMember = manifest.members.find(member => member.role === "config");
      const authMember = manifest.members.find(member => member.role === "auth");
      if (!configMember || !authMember) throw new Error("invalid_manifest_members");
      configMember.bytes = config.byteLength; configMember.sha256 = sha256(config);
      const auth = await readFile(join(root, "neo4j.auth")); authMember.bytes = auth.byteLength; authMember.sha256 = sha256(auth);
    },
    // A backup only restarts the fenced source on its new ephemeral port.
    startAndReady: async (_root: string, epoch: string) => {
      if (await dockerOutput(["inspect", "--format", "{{.State.Running}}", container]) !== "true") await dockerOutput(["start", container]);
      const mapped = await dockerOutput(["port", container, "7687/tcp"]);
      const uri = `bolt://127.0.0.1:${Number(mapped.split(":").at(-1))}`;
      process.env["ANAMNESIS_NEO4J_URI"] = uri;
      await waitBolt(uri, neo4jUser(), neo4jPassword());
      return { sourceId: installation.incarnation, epoch, ready: true };
    },
    // A restore verifies the database it loaded under `<root>/database`, never the fenced source.
    startRestored: async (root: string, epoch: string) => {
      restored = await startRestoredDatabase(root, owner, neo4jPassword());
      return { sourceId: installation.incarnation, epoch, ready: true };
    },
    rebindSource: async () => {},
    verifyPhysicalLinks: async () => {},
    // The quarantined tree keeps its data; the container that was serving it must not stay up.
    // The binding is dropped only once removal succeeded, so a failed removal stays addressable.
    quarantine: async () => { if (restored) { await removeContainer(restored.container); restored = undefined; } },
  };
  const lifecycle = { stop: async () => {} };
  return new OwnedNeo4jAdapter({ container, owner, authority, lifecycle });
}

export async function objectInventory(root: string): Promise<ArchiveManifest["objects"]> {
  const objects: ArchiveManifest["objects"] = [];
  for (const prefix of (await readdir(root)).sort()) {
    if (!/^[0-9a-f]{2}$/.test(prefix)) continue;
    for (const name of (await readdir(join(root, prefix))).sort()) {
      if (!/^[0-9a-f]{64}$/.test(name)) continue;
      const bytes = await readFile(join(root, prefix, name));
      const sidecar = JSON.parse(await readFile(join(root, prefix, `${name}.json`), "utf8"));
      if (sha256(bytes) !== name) throw new Error("object_corrupt");
      objects.push({ hash: name, size: bytes.byteLength, media_type: sidecar.mediaType });
    }
  }
  return objects.sort((a, b) => a.hash.localeCompare(b.hash));
}

export function manifestTemplate(operationId: string, cutoff: ArchiveManifest["cutoff"], authority: AuthoritySnapshot, objects: ArchiveManifest["objects"], configSha256: string): ArchiveManifest {
  const imageDigest = NEO4J_IMAGE.slice("neo4j@".length);
  const members = [
    { path: "config.jsonc", role: "config" as const, bytes: 1, sha256: configSha256 },
    { path: "database/neo4j.dump", role: "database_dump" as const, bytes: 1, sha256: "0".repeat(64) },
    { path: "database/neo4j.dump.metadata.json", role: "dump_metadata" as const, bytes: 1, sha256: "0".repeat(64) },
    { path: "neo4j.auth", role: "auth" as const, bytes: 1, sha256: "0".repeat(64) },
    ...objects.flatMap(object => [
      { path: `objects/${object.hash.slice(0, 2)}/${object.hash}`, role: "object_data" as const, bytes: object.size, sha256: object.hash },
      { path: `objects/${object.hash.slice(0, 2)}/${object.hash}.json`, role: "object_sidecar" as const, bytes: 1, sha256: "0".repeat(64) },
    ]),
  ].sort((a, b) => a.path.localeCompare(b.path));
  return { format: "anamnesis.archive/1", operation_id: operationId, cutoff,
    compatibility: { schema_version: "anamnesis.storage/1", neo4j_version: NEO4J_VERSION, neo4j_image_digest: imageDigest, episode_digest_version_ceiling: 2 },
    configuration: { config_sha256: configSha256, receipt_retention_ms: 3650 * 86400000, prior_version: "runtime/1", calibration_version: "runtime/1", dynamics_version: "runtime/1" },
    models: { active_embedding_profile_id: null, embedding_profiles: [], embedding_coverages: [], extraction: null }, objects, members, authority };
}
