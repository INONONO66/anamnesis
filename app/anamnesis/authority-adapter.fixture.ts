import { createHash } from "node:crypto";
import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { AuthoritySnapshot } from "@anamnesis/core";
import type { TrustedAuthorityAdapter } from "./backup-restore-orchestrator.ts";
import { NEO4J_IMAGE, NEO4J_VERSION } from "./owned-neo4j-adapter.ts";

export const sha256 = (bytes: Uint8Array | string) => createHash("sha256").update(bytes).digest("hex");
export const fixtureCutoff = { ingest_seq: 7, structure_revision: 11, policy_revision: 3 };
export const FIXTURE_IMAGE_DIGEST = NEO4J_IMAGE.slice("neo4j@".length);
export const FIXTURE_CONFIG = Buffer.from('{"fixture":true}\n');
const FIXTURE_AUTH = Buffer.from("neo4j/fixture-not-a-live-secret\n");

export function fixtureAuthority(): AuthoritySnapshot {
  return { members: ["episode-1"], retained_generations: [1], coverage: { ...fixtureCutoff }, physical_links: [], invalidation_evidence: [], source_hashes: ["a".repeat(64)] };
}

type Recorder = (name: string) => void;
export type AdapterOverrides = (record: Recorder) => Partial<TrustedAuthorityAdapter>;
export const noOverrides: AdapterOverrides = () => ({});

export interface FixtureOptions { overrides?: AdapterOverrides; config?: Buffer }
export function fakeAuthorityAdapter(sourceId: string, { overrides = noOverrides, config = FIXTURE_CONFIG }: FixtureOptions = {}) {
  const calls: string[] = [];
  const record: Recorder = name => { calls.push(name); };
  const adapter: TrustedAuthorityAdapter = {
    revokeWriters: async () => { record("revokeWriters"); return { epoch: "1", cutoff: { ...fixtureCutoff } }; },
    authoritySnapshot: async () => { record("authoritySnapshot"); return fixtureAuthority(); },
    dumpOffline: async destination => {
      record("dumpOffline");
      await writeFile(destination, Buffer.from("neo4j dump"), { flag: "wx", mode: 0o600 });
      return { metadata: new Uint8Array(), neo4jVersion: NEO4J_VERSION, imageDigest: FIXTURE_IMAGE_DIGEST };
    },
    materializeMembers: async (root, target) => {
      record("materializeMembers");
      for (const [path, role, bytes] of [["config.jsonc", "config", config], ["neo4j.auth", "auth", FIXTURE_AUTH]] as const) {
        await writeFile(join(root, path), bytes, { flag: "wx", mode: 0o600 });
        const member = target.members.find(m => m.role === role);
        if (!member) throw new Error(`missing ${role} member`);
        member.bytes = bytes.byteLength; member.sha256 = sha256(bytes);
      }
    },
    startAndReady: async (_root, epoch) => { record("startAndReady"); return { sourceId, epoch, ready: true }; },
    stop: async () => { record("stop"); },
    restoreOffline: async () => { record("restoreOffline"); },
    rebindSource: async () => { record("rebindSource"); },
    verifyPhysicalLinks: async () => { record("verifyPhysicalLinks"); },
    quarantine: async root => { record(`quarantine:${root}`); },
    ...overrides(record),
  };
  return { adapter, calls };
}
