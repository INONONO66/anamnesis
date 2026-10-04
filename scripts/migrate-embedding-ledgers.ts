import { mkdir, readFile, rename, writeFile } from "node:fs/promises";
import { homedir } from "node:os";
import { join } from "node:path";
import neo4j from "neo4j-driver";
import {
  RpcEmbeddingAttempt,
  type RpcEmbeddingAttempt as EmbeddingAttempt,
} from "../packages/protocol/src/rpc.ts";

const USAGE = "Usage: bun scripts/migrate-embedding-ledgers.ts [--dry-run] [--runtime-root <dir>] [--batch <positive integer>]";

type LedgerEntry = {
  readonly profile_id: string;
  readonly state: "deferred" | "quarantined";
  readonly deferrals: number;
  readonly retry_after: number | null;
  readonly attempts: EmbeddingAttempt[];
};

type LedgerFile = {
  readonly version: 1;
  readonly episodes: Record<string, LedgerEntry>;
};

type Candidate = {
  readonly episodeId: string;
  readonly profileId: string;
  readonly operationId: string;
  readonly attempt: EmbeddingAttempt;
};

type MigrationArguments = {
  readonly dryRun: boolean;
  readonly runtimeRoot: string;
  readonly batch: number;
};

export type MigrationOptions = {
  readonly args?: readonly string[];
  readonly uri?: string;
  readonly user?: string;
  readonly password?: string;
  readonly database?: string;
  readonly output?: (line: string) => void;
};

export type MigrationResult = {
  readonly seeded: number;
  readonly skippedWithVector: number;
  readonly alreadyPresent: number;
  readonly embeddingAttempts: number;
  readonly outbox: number;
};

function parseArguments(args: readonly string[]): MigrationArguments {
  let dryRun = false;
  let runtimeRoot = process.env["ANAMNESIS_RUNTIME_ROOT"] ?? join(homedir(), ".anamnesis");
  let batch = 10_000;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--dry-run") {
      dryRun = true;
      continue;
    }
    if (argument === "--runtime-root" || argument === "--batch") {
      const value = args[index + 1];
      if (!value || value.startsWith("--")) throw new Error(`Missing value for ${argument}\n${USAGE}`);
      index += 1;
      if (argument === "--runtime-root") runtimeRoot = value;
      else {
        if (!/^[1-9]\d*$/.test(value)) throw new Error(`--batch must be a positive integer\n${USAGE}`);
        batch = Number(value);
        if (!Number.isSafeInteger(batch)) throw new Error(`--batch is out of bounds\n${USAGE}`);
      }
      continue;
    }
    throw new Error(`Unknown option: ${argument}\n${USAGE}`);
  }
  return { dryRun, runtimeRoot, batch };
}

function required(value: string | undefined, name: string): string {
  if (!value) throw new Error(`${name} is required to run the embedding ledger migration`);
  return value;
}

function numberValue(value: unknown, name: string): number {
  if (typeof value !== "number") throw new Error(`Neo4j returned a non-numeric ${name}`);
  return value;
}

function stringValue(value: unknown, name: string): string {
  if (typeof value !== "string") throw new Error(`Neo4j returned a non-string ${name}`);
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return !!value && typeof value === "object" && !Array.isArray(value);
}

function recordValue(value: unknown, name: string): Record<string, unknown> {
  if (!isRecord(value)) throw new Error(`Invalid ${name} in embedding-state.json`);
  return value;
}

function parseLedger(value: unknown): LedgerFile {
  const document = recordValue(value, "document");
  if (document["version"] !== 1) throw new Error("embedding-state.json must use ledger version 1");
  const sourceEpisodes = recordValue(document["episodes"], "episodes");
  const episodes: Record<string, LedgerEntry> = {};
  for (const [episodeId, source] of Object.entries(sourceEpisodes)) {
    const entry = recordValue(source, `episode ${episodeId}`);
    const state = entry["state"];
    const profileId = entry["profile_id"];
    const deferrals = entry["deferrals"];
    const retryAfter = entry["retry_after"];
    const sourceAttempts = entry["attempts"];
    if (
      (state !== "deferred" && state !== "quarantined")
      || typeof profileId !== "string"
      || !/^[0-9a-f]{64}$/.test(profileId)
      || typeof deferrals !== "number"
      || !Number.isInteger(deferrals)
      || deferrals < 0
      || (retryAfter !== null && typeof retryAfter !== "number")
      || !Array.isArray(sourceAttempts)
      || sourceAttempts.length < 1
      || sourceAttempts.length > 16
    ) throw new Error(`Invalid ledger entry for episode ${episodeId}`);
    const attempts = sourceAttempts.map((attempt) => RpcEmbeddingAttempt.parse(attempt));
    episodes[episodeId] = {
      profile_id: profileId,
      state,
      deferrals,
      retry_after: retryAfter === null ? null : numberValue(retryAfter, "retry_after"),
      attempts,
    };
  }
  return { version: 1, episodes };
}

function latestQuarantined(candidates: readonly Candidate[]): Candidate[] {
  const latest = new Map<string, Candidate>();
  for (const candidate of candidates) {
    const current = latest.get(candidate.episodeId);
    if (
      !current
      || candidate.attempt.created_at > current.attempt.created_at
      || (
        candidate.attempt.created_at === current.attempt.created_at
        && candidate.operationId > current.operationId
      )
    ) latest.set(candidate.episodeId, candidate);
  }
  return [...latest.values()].sort((left, right) => left.episodeId.localeCompare(right.episodeId));
}

async function readLedger(path: string): Promise<LedgerFile> {
  try {
    return parseLedger(JSON.parse(await readFile(path, "utf8")));
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "ENOENT") {
      return { version: 1, episodes: {} };
    }
    throw error;
  }
}

async function writeLedger(path: string, ledger: LedgerFile): Promise<void> {
  await mkdir(join(path, ".."), { recursive: true });
  const temporary = `${path}.tmp`;
  await writeFile(temporary, `${JSON.stringify(ledger, null, 2)}\n`, "utf8");
  await rename(temporary, path);
}

export async function main(options: MigrationOptions = {}): Promise<MigrationResult> {
  const args = parseArguments(options.args ?? process.argv.slice(2));
  const output = options.output ?? console.log;
  const uri = required(options.uri ?? process.env["ANAMNESIS_NEO4J_URI"], "ANAMNESIS_NEO4J_URI");
  const user = required(options.user ?? process.env["ANAMNESIS_NEO4J_USER"], "ANAMNESIS_NEO4J_USER");
  const password = required(options.password ?? process.env["ANAMNESIS_NEO4J_PASSWORD"], "ANAMNESIS_NEO4J_PASSWORD");
  const database = options.database ?? process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j";
  const driver = neo4j.driver(uri, neo4j.auth.basic(user, password), { disableLosslessIntegers: true });
  const session = driver.session({ database });
  const line = (step: string, values: Record<string, unknown>) => output(JSON.stringify({ step, ...values }));

  try {
    const inspection = await session.run<{ embeddingAttempts: number; outbox: number }>(`
      CALL { MATCH (a:EmbeddingAttempt) RETURN count(a) AS embeddingAttempts }
      CALL { MATCH (o:Outbox) RETURN count(o) AS outbox }
      RETURN embeddingAttempts, outbox
    `);
    const inspected = inspection.records[0];
    if (!inspected) throw new Error("Neo4j returned no migration inspection row");
    const embeddingAttempts = numberValue(inspected.get("embeddingAttempts"), "EmbeddingAttempt count");
    const outbox = numberValue(inspected.get("outbox"), "Outbox count");
    // Neo4j 5 rejects SHOW inside a CALL subquery, so the schema checks are standalone statements.
    const constraints = await session.run<{ name: string }>(
      "SHOW CONSTRAINTS YIELD name WHERE name = 'embedding_attempt_id' RETURN name",
    );
    const indexes = await session.run<{ name: string }>(
      "SHOW INDEXES YIELD name WHERE name = 'outbox_pending' RETURN name",
    );
    line("inspect", {
      embedding_attempts: embeddingAttempts,
      outbox,
      embedding_attempt_constraint: constraints.records.length > 0,
      outbox_pending_index: indexes.records.length > 0,
    });

    const quarantined = await session.run<{
      episodeId: string;
      profileId: string;
      operationId: string;
      body: string;
    }>(`
      MATCH (a:EmbeddingAttempt { state: 'quarantined' })
      RETURN a.episode_id AS episodeId, a.profile_id AS profileId,
             a.operation_id AS operationId, a.body AS body
    `);
    const candidates = latestQuarantined(quarantined.records.map((record) => ({
      episodeId: stringValue(record.get("episodeId"), "episode_id"),
      profileId: stringValue(record.get("profileId"), "profile_id"),
      operationId: stringValue(record.get("operationId"), "operation_id"),
      attempt: RpcEmbeddingAttempt.parse(JSON.parse(stringValue(record.get("body"), "body"))),
    })));
    const vectors = await session.run<{ episodeId: string; profileId: string; hasVector: boolean }>(`
      UNWIND $candidates AS candidate
      OPTIONAL MATCH (v:EmbeddingVector {
        episode_id: candidate.episode_id,
        profile_id: candidate.profile_id
      })
      RETURN candidate.episode_id AS episodeId, candidate.profile_id AS profileId,
             count(v) > 0 AS hasVector
    `, {
      candidates: candidates.map((candidate) => ({
        episode_id: candidate.episodeId,
        profile_id: candidate.profileId,
      })),
    });
    const hasVector = new Set(vectors.records.flatMap((record) => (
      record.get("hasVector") === true
        ? [`${stringValue(record.get("episodeId"), "episode_id")}\u0000${stringValue(record.get("profileId"), "profile_id")}`]
        : []
    )));
    const ledgerPath = join(args.runtimeRoot, "embedding-state.json");
    const ledger = await readLedger(ledgerPath);
    let seeded = 0;
    let skippedWithVector = 0;
    let alreadyPresent = 0;
    for (const candidate of candidates) {
      const key = `${candidate.episodeId}\u0000${candidate.profileId}`;
      if (hasVector.has(key)) {
        skippedWithVector += 1;
      } else if (ledger.episodes[candidate.episodeId]) {
        alreadyPresent += 1;
      } else {
        ledger.episodes[candidate.episodeId] = {
          profile_id: candidate.profileId,
          state: "quarantined",
          deferrals: 0,
          retry_after: null,
          attempts: [candidate.attempt],
        };
        seeded += 1;
      }
    }
    if (!args.dryRun && seeded > 0) await writeLedger(ledgerPath, ledger);
    line("seed_ledger", { seeded, skipped_with_vector: skippedWithVector, already_present: alreadyPresent, dry_run: args.dryRun });

    if (args.dryRun) {
      line("drop_schema", { dry_run: true, constraints: ["embedding_attempt_id"], indexes: ["outbox_pending"] });
      line("delete_legacy_ledgers", { dry_run: true, embedding_attempts: embeddingAttempts, outbox });
      return { seeded, skippedWithVector, alreadyPresent, embeddingAttempts, outbox };
    }

    await session.run("DROP CONSTRAINT embedding_attempt_id IF EXISTS");
    await session.run("DROP INDEX outbox_pending IF EXISTS");
    line("drop_schema", { dropped_constraint: "embedding_attempt_id", dropped_index: "outbox_pending" });
    await session.run(`
      MATCH (n)
      WHERE n:EmbeddingAttempt OR n:Outbox
      CALL (n) { DETACH DELETE n } IN TRANSACTIONS OF ${args.batch} ROWS
    `);
    line("delete_legacy_ledgers", { embedding_attempts: embeddingAttempts, outbox, batch: args.batch });

    const finalCounts = await session.run<{ embeddingAttempts: number; outbox: number }>(`
      CALL { MATCH (a:EmbeddingAttempt) RETURN count(a) AS embeddingAttempts }
      CALL { MATCH (o:Outbox) RETURN count(o) AS outbox }
      RETURN embeddingAttempts, outbox
    `);
    const final = finalCounts.records[0];
    if (!final) throw new Error("Neo4j returned no final migration count row");
    const finalEmbeddingAttempts = numberValue(final.get("embeddingAttempts"), "final EmbeddingAttempt count");
    const finalOutbox = numberValue(final.get("outbox"), "final Outbox count");
    line("final_counts", { embedding_attempts: finalEmbeddingAttempts, outbox: finalOutbox });
    if (finalEmbeddingAttempts !== 0 || finalOutbox !== 0) {
      throw new Error(`Legacy ledger deletion incomplete: EmbeddingAttempt=${finalEmbeddingAttempts}, Outbox=${finalOutbox}`);
    }
    return { seeded, skippedWithVector, alreadyPresent, embeddingAttempts: finalEmbeddingAttempts, outbox: finalOutbox };
  } finally {
    await session.close();
    await driver.close();
  }
}

if (import.meta.main) {
  // no-excuse-ok: catch
  main().catch((error: unknown) => {
    console.error(error instanceof Error ? error.message : String(error));
    process.exitCode = 1;
  });
}
