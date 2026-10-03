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

const sha256 = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
const dockerExec = promisify(execFile);
const dockerOutput = async (args: string[]) => (await dockerExec("docker", args)).stdout.trim();

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
  let restoredUri: string | undefined;
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
      if (!restoredUri) throw new Error("restore_not_started");
      return readRestoredAuthority({ uri: restoredUri, user: process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j",
        password: process.env["ANAMNESIS_NEO4J_PASSWORD"] ?? "", database: process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j" }, context);
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
    startAndReady: async (_root: string, epoch: string) => {
      if (await dockerOutput(["inspect", "--format", "{{.State.Running}}", container]) !== "true") await dockerOutput(["start", container]);
      const mapped = await dockerOutput(["port", container, "7687/tcp"]);
      const port = Number(mapped.split(":").at(-1));
      const uri = `bolt://127.0.0.1:${port}`;
      process.env["ANAMNESIS_NEO4J_URI"] = uri;
      const driver = neo4j.driver(uri, neo4j.auth.basic(process.env["ANAMNESIS_NEO4J_USER"] ?? "neo4j", process.env["ANAMNESIS_NEO4J_PASSWORD"] ?? ""), { connectionTimeout: 1000, connectionAcquisitionTimeout: 1500, maxTransactionRetryTime: 0 });
      try {
        const deadline = Date.now() + 90000;
        while (true) {
          try { await driver.verifyConnectivity(); restoredUri = uri; return { sourceId: installation.incarnation, epoch, ready: true }; }
          catch (error) { if (Date.now() >= deadline) throw error; }
        }
      } finally { await driver.close(); }
    },
    rebindSource: async () => {},
    verifyPhysicalLinks: async () => {},
    quarantine: async () => {},
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
