import neo4j from "neo4j-driver";
import { canonicalExtractionBody, extractionBodyDigest, EchoLineage } from "@anamnesis/protocol";

const LABELS = ["MaterializationOperation", "OriginHead", "EchoLineage", "FactRelationInput", "FactRelationVerdict", "EntityWitness"] as const;
const USAGE = "Usage: node dist/anamnesis-migrate-g4.mjs [--dry-run] [--batch <positive integer>]";
type Counts = Record<typeof LABELS[number], number>;
type Cursor = { readonly generation: string; readonly episodes: number; readonly active_extraction: number };
type Schema = { readonly name: string; readonly labelsOrTypes: string[] | null; readonly owningConstraint?: string | null };
type Options = {
  readonly args?: readonly string[];
  readonly uri?: string;
  readonly user?: string;
  readonly password?: string;
  readonly database?: string;
  readonly output?: (line: string) => void;
};
function required(value: string | undefined, name: string): string {
  if (!value) throw new Error(`${name} is required to run the materialization ledger migration`);
  return value;
}

export async function main(options: Options = {}) {
  let dryRun = false, batch = 10_000;
  const args = options.args ?? process.argv.slice(2);
  for (let index = 0; index < args.length; index++) {
    if (args[index] === "--dry-run") dryRun = true;
    else if (args[index] === "--batch") {
      const value = args[++index];
      if (!value || !/^[1-9]\d*$/.test(value) || !Number.isSafeInteger(Number(value))) throw new Error(`--batch must be a positive safe integer\n${USAGE}`);
      batch = Number(value);
    } else throw new Error(`Unknown option: ${args[index]}\n${USAGE}`);
  }
  const uri = required(options.uri ?? process.env["ANAMNESIS_NEO4J_URI"], "ANAMNESIS_NEO4J_URI");
  const user = required(options.user ?? process.env["ANAMNESIS_NEO4J_USER"], "ANAMNESIS_NEO4J_USER");
  const password = required(options.password ?? process.env["ANAMNESIS_NEO4J_PASSWORD"], "ANAMNESIS_NEO4J_PASSWORD");
  const database = options.database ?? process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j";
  const line = (step: string, fields: Record<string, unknown>) => (options.output ?? console.log)(JSON.stringify({ step, ...fields }));
  const driver = neo4j.driver(uri, neo4j.auth.basic(user, password), { disableLosslessIntegers: true });
  const session = driver.session({ database });
  const number = async (query: string): Promise<number> => {
    const value = (await session.run<{ count: number }>(query)).records[0]?.get("count");
    if (typeof value !== "number") throw new Error(`Non-numeric count from ${query}`);
    return value;
  };
  const counts = async (): Promise<Counts> => {
    const values: [string, number][] = [];
    for (const label of LABELS) values.push([label, await number(`MATCH (n:${label}) WITH count(n) AS count RETURN count`)]);
    return Object.fromEntries(values) as Counts;
  };
  const schema = async () => {
    // SHOW must be a standalone statement in Neo4j 5. Constraint-backed indexes are dropped with the constraint.
    const constraints = (await session.run<Schema>("SHOW CONSTRAINTS YIELD name, labelsOrTypes RETURN name, labelsOrTypes")).records
      .map(row => ({ name: row.get("name"), labelsOrTypes: row.get("labelsOrTypes") }))
      .filter(row => row.labelsOrTypes?.some(label => LABELS.some(legacy => legacy === label)) ?? false);
    const indexes = (await session.run<Schema>("SHOW INDEXES YIELD name, labelsOrTypes, owningConstraint RETURN name, labelsOrTypes, owningConstraint")).records
      .map(row => ({ name: row.get("name"), labelsOrTypes: row.get("labelsOrTypes"), owningConstraint: row.get("owningConstraint") }))
      .filter(row => row.labelsOrTypes?.some(label => LABELS.some(legacy => legacy === label)) ?? false);
    return { constraints, indexes };
  };
  const cursors = async (): Promise<Cursor[]> => {
    const rows = await session.run<{ generation: string; partitions: { partition: unknown; covered: unknown }[] }>(
      `MATCH (g:ExtractionGeneration) OPTIONAL MATCH (c:ExtractionCoverage {generation_id:g.id})
       RETURN g.id AS generation,collect({partition:c.partition,covered:c.covered_ingest_seq}) AS partitions ORDER BY generation`);
    return rows.records.map(row => {
      const cursor = (partition: string) => {
        const value = row.get("partitions").find(part => part.partition === partition)?.covered ?? 0;
        if (typeof value !== "number") throw new Error(`Non-numeric ${partition} cursor for ${row.get("generation")}`);
        return value;
      };
      return { generation: row.get("generation"), episodes: cursor("episodes"), active_extraction: cursor("active_extraction") };
    });
  };
  // Check destructive-cutover gates before the first copy write; emit them in the requested step order below.
  const inflight = async () => {
    const operations = (await session.run<{ id: string }>(`MATCH (o:MaterializationOperation)
      WHERE o.result IS NULL OR o.occurrence_key IS NULL
      RETURN o.id AS id ORDER BY id LIMIT 100`)).records.map(row => row.get("id"));
    const operationCount = await number(`MATCH (o:MaterializationOperation)
      WHERE o.result IS NULL OR o.occurrence_key IS NULL RETURN count(o) AS count`);
    const zeroCandidate = await number(`MATCH (i:FactRelationInput)
      WHERE i.candidates = 0 RETURN count(i) AS count`);
    const sealed = new Set<string>();
    const sealedRows = await session.run<{ generation: string; source: string; occurrence: string; result: string }>(
      `MATCH (o:MaterializationOperation)
       WHERE o.result CONTAINS '"omitted":"relation_judge_exhausted"' AND o.occurrence_key IS NOT NULL
         AND o.generation IS NOT NULL AND o.source_episode_id IS NOT NULL
       RETURN o.generation AS generation,o.source_episode_id AS source,o.occurrence_key AS occurrence,o.result AS result`);
    for (const row of sealedRows.records) {
      let result: unknown;
      try { result = JSON.parse(row.get("result")); } catch { continue; }
      if (typeof result === "object" && result !== null && "omitted" in result
        && result.omitted === "relation_judge_exhausted"
        && row.get("occurrence") === extractionBodyDigest([row.get("generation"), row.get("source")]))
        sealed.add(`${row.get("generation")}:${row.get("source")}`);
    }
    const pendingRows = await session.run<{ id: string; generation: string; source: string }>(`MATCH (i:FactRelationInput)
      WHERE i.candidates > 0 AND NOT EXISTS { MATCH (:FactRelationVerdict {occurrence_key:i.occurrence_key}) }
      RETURN i.occurrence_key AS id,i.generation AS generation,i.source_episode_id AS source ORDER BY id`);
    const pending: string[] = [], sealedExhausted: string[] = [];
    let pendingCount = 0, sealedCount = 0;
    for (const row of pendingRows.records) {
      const id = row.get("id"), sourceKey = `${row.get("generation")}:${row.get("source")}`;
      if (sealed.has(sourceKey)) {
        sealedCount++;
        if (sealedExhausted.length < 100) sealedExhausted.push(id);
      } else {
        pendingCount++;
        if (pending.length < 100) pending.push(id);
      }
    }
    return {
      operations, inputs: pending, count: operationCount + pendingCount,
      terminal_zero_candidate: zeroCandidate, sealed_exhausted: sealedCount, pending: pendingCount,
      sealed_inputs: sealedExhausted,
    };
  };
  try {
    const before = await counts(), legacySchema = await schema();
    const facts = await number("MATCH (f:Fact) RETURN count(f) AS count");
    line("count_legacy", { counts: before, constraints: legacySchema.constraints.map(c => c.name), indexes: legacySchema.indexes.map(i => i.name), facts, dry_run: dryRun });
    const coverage = await cursors(), split = coverage.filter(row => row.episodes !== row.active_extraction);
    const pending = await inflight();
    const duplicates = (await session.run<{ generation: string; meaning_digest: string; primary_episode_id: string; ids: string[] }>(
      `MATCH (f:Fact) WHERE f.generation IS NOT NULL AND f.meaning_digest IS NOT NULL AND f.primary_episode_id IS NOT NULL
       WITH f.generation AS generation,f.meaning_digest AS meaning_digest,f.primary_episode_id AS primary_episode_id,collect(f.id) AS ids
       WHERE size(ids)>1 RETURN generation,meaning_digest,primary_episode_id,ids LIMIT 100`)).records.map(row => row.toObject());
    const duplicateCount = await number(`MATCH (f:Fact)
      WHERE f.generation IS NOT NULL AND f.meaning_digest IS NOT NULL AND f.primary_episode_id IS NOT NULL
      WITH f.generation AS generation,f.meaning_digest AS meaning_digest,f.primary_episode_id AS primary_episode_id,count(f) AS duplicates
      WHERE duplicates > 1 RETURN count(*) AS count`);
    const witnessMissing = (await session.run<{ id: string }>(`MATCH (w:EntityWitness)
      OPTIONAL MATCH (e:Entity) WHERE e.id = w.entity_id
      WITH w,e
      WHERE w.state <> 'COMPLETE' OR w.state IS NULL OR e IS NULL
        OR (e.witness_generation IS NOT NULL AND
          (NOT coalesce(e.witness_generation = w.generation,false)
           OR NOT coalesce(e.witness_policy_revision = w.policy_revision,false)))
      RETURN w.entity_id AS id LIMIT 20`)).records.map(row => row.get("id"));
    const witnessJoined = await number(`MATCH (w:EntityWitness {state:'COMPLETE'}) MATCH (e:Entity) WHERE e.id = w.entity_id
      RETURN count(DISTINCT e) AS count`);
    const existingWitness = await number(`MATCH (e:Entity) WHERE e.witness_generation IS NOT NULL RETURN count(e) AS count`);

    // Check every join before any write, including the live copy. A missing Episode must never be hidden by a
    // count of pre-existing lineage properties on an unrelated Episode.
    const lineageMissing = await session.run<{ id: string; episode: string | null; mode: string | null }>(`MATCH (l:EchoLineage)
      WHERE NOT EXISTS { MATCH (e:Episode) WHERE e.id = l.episode_id }
        OR EXISTS { MATCH (e:Episode) WHERE e.id = l.episode_id
          AND (e.lineage_mode IS NOT NULL AND
            (NOT coalesce(e.lineage_mode = l.lineage_mode,false) OR NOT coalesce(e.parent_recall_ids = l.parent_recall_ids,false)
             OR NOT coalesce(e.context_digests = l.context_digests,false) OR NOT coalesce(e.root_episode_ids = l.root_episode_ids,false)
             OR NOT coalesce(e.echo_depth = l.echo_depth,false) OR NOT coalesce(e.lineage_complete = l.complete,false))) }
      RETURN l.episode_id AS id,null AS episode,null AS mode LIMIT 20`);
    const lineageJoined = await number(`MATCH (l:EchoLineage) MATCH (e:Episode) WHERE e.id = l.episode_id
      RETURN count(DISTINCT e) AS count`);
    const existingLineage = await number(`MATCH (e:Episode) WHERE e.lineage_mode IS NOT NULL RETURN count(e) AS count`);
    const unrelatedLineage = (await session.run<{ id: string }>(`MATCH (e:Episode)
      WHERE e.lineage_mode IS NOT NULL AND NOT EXISTS { MATCH (l:EchoLineage {episode_id:e.id}) }
      RETURN e.id AS id LIMIT 20`)).records.map(row => row.get("id"));
    const badBodies: string[] = [];
    let lineageCursor = "", lineageValidated = 0;
    while (true) {
      const rows = await session.run<{
        digest: string; body: string; episode_id: string; lineage_mode: string; parent_recall_ids: string[];
        context_digests: string[]; root_episode_ids: string[]; echo_depth: number; complete: boolean; episode_digest: string | null;
      }>(
        `MATCH (l:EchoLineage)
         WHERE l.episode_id > $cursor
         MATCH (e:Episode {id:l.episode_id})
         RETURN l.digest AS digest,l.body AS body,l.episode_id AS episode_id,l.lineage_mode AS lineage_mode,
           l.parent_recall_ids AS parent_recall_ids,l.context_digests AS context_digests,l.root_episode_ids AS root_episode_ids,
           l.echo_depth AS echo_depth,l.complete AS complete,e.lineage_digest AS episode_digest
         ORDER BY l.episode_id LIMIT ${batch}`, { cursor: lineageCursor });
      if (rows.records.length === 0) break;
      for (const row of rows.records) {
        try {
          const parsed = EchoLineage.parse(JSON.parse(row.get("body")));
          const stored = {
            episode_id: row.get("episode_id"), lineage_mode: row.get("lineage_mode"),
            parent_recall_ids: row.get("parent_recall_ids"), context_digests: row.get("context_digests"),
            root_episode_ids: row.get("root_episode_ids"), echo_depth: row.get("echo_depth"), complete: row.get("complete"),
          };
          const digest = extractionBodyDigest(parsed);
          if (canonicalExtractionBody(parsed) !== row.get("body") || canonicalExtractionBody(parsed) !== canonicalExtractionBody(stored)
            || digest !== row.get("digest") || (row.get("episode_digest") !== null && row.get("episode_digest") !== digest))
            if (badBodies.length < 100) badBodies.push(`${parsed.episode_id}:body-or-digest`);
        } catch (error) {
          if (badBodies.length < 100) badBodies.push(`${row.get("episode_id")}:${String(error)}`);
        }
      }
      lineageValidated += rows.records.length;
      lineageCursor = rows.records[rows.records.length - 1]?.get("episode_id") ?? lineageCursor;
    }
    if (lineageValidated !== before.EchoLineage && badBodies.length < 100) badBodies.push(`joined:${lineageValidated}/${before.EchoLineage}`);
    const missingLineage = lineageMissing.records.map(row => `${row.get("id")}:episode=${row.get("episode")}:mode=${row.get("mode")}`);
    if (lineageJoined !== before.EchoLineage || existingLineage > before.EchoLineage || unrelatedLineage.length || missingLineage.length || badBodies.length) {
      const ids = [...unrelatedLineage.map(id => `${id}:unrelated-lineage`), ...missingLineage, ...badBodies];
      line("copy_mismatch", { label: "EchoLineage", count: lineageJoined, existing: existingLineage, expected: before.EchoLineage, ids, dry_run: dryRun });
      throw new Error(`copy_mismatch: EchoLineage has missing Episodes, conflicting properties or invalid bodies: ${JSON.stringify(ids)}`);
    }
    // All destructive-cutover gates are read before copying, without obscuring the ordered output.
    const gatesPass = split.length === 0 && pending.count === 0 && duplicateCount === 0
      && witnessMissing.length === 0 && witnessJoined === before.EntityWitness && existingWitness <= before.EntityWitness;
    if (!dryRun && gatesPass) {
      await session.run(`MATCH (l:EchoLineage) CALL (l) {
        MATCH (e:Episode) WHERE e.id = l.episode_id
        SET e.lineage_mode=l.lineage_mode,e.parent_recall_ids=l.parent_recall_ids,
            e.context_digests=l.context_digests,e.root_episode_ids=l.root_episode_ids,
            e.echo_depth=l.echo_depth,e.lineage_complete=l.complete,e.lineage_digest=l.digest
      } IN TRANSACTIONS OF ${batch} ROWS`);
    }
    const lineageCount = dryRun || !gatesPass ? before.EchoLineage : await number("MATCH (e:Episode) WHERE e.lineage_mode IS NOT NULL RETURN count(e) AS count");
    const postcopyMismatch: string[] = [];
    let postcopyMismatchCount = 0;
    if (!dryRun && gatesPass) {
      let cursor = "", verified = 0;
      while (true) {
        const rows = await session.run<{ id: string; digest: string | null; body: string; matches: boolean }>(
          `MATCH (l:EchoLineage) MATCH (e:Episode {id:l.episode_id})
           WHERE l.episode_id > $cursor
           RETURN l.episode_id AS id,e.lineage_digest AS digest,l.body AS body,
             coalesce(e.lineage_mode = l.lineage_mode,false) AND coalesce(e.parent_recall_ids = l.parent_recall_ids,false)
             AND coalesce(e.context_digests = l.context_digests,false) AND coalesce(e.root_episode_ids = l.root_episode_ids,false)
             AND coalesce(e.echo_depth = l.echo_depth,false) AND coalesce(e.lineage_complete = l.complete,false) AS matches
           ORDER BY l.episode_id LIMIT ${batch}`, { cursor });
        if (rows.records.length === 0) break;
        for (const row of rows.records) {
          const digest = extractionBodyDigest(EchoLineage.parse(JSON.parse(row.get("body"))));
          if (row.get("digest") !== digest || !row.get("matches")) {
            postcopyMismatchCount++;
            if (postcopyMismatch.length < 100) postcopyMismatch.push(row.get("id"));
          }
        }
        verified += rows.records.length;
        cursor = rows.records[rows.records.length - 1]?.get("id") ?? cursor;
      }
      if (verified !== before.EchoLineage) {
        postcopyMismatchCount++;
        if (postcopyMismatch.length < 100) postcopyMismatch.push(`count:${verified}/${before.EchoLineage}`);
      }
    }
    if (lineageCount !== before.EchoLineage || postcopyMismatchCount) {
      line("copy_mismatch", { label: "EchoLineage", count: lineageCount, expected: before.EchoLineage, ids: postcopyMismatch });
      throw new Error("copy_mismatch: Episode lineage count or digest differs");
    }
    line("copy_lineage", { count: lineageCount, validated: lineageValidated, mismatch: postcopyMismatchCount, dry_run: dryRun });

    if (witnessJoined !== before.EntityWitness || existingWitness > before.EntityWitness || witnessMissing.length) {
      line("copy_mismatch", { label: "EntityWitness", count: witnessJoined, existing: existingWitness, expected: before.EntityWitness, ids: witnessMissing, dry_run: dryRun });
      throw new Error("copy_mismatch: EntityWitness has incomplete, missing or conflicting Entities");
    }
    if (!dryRun && gatesPass) await session.run(`MATCH (w:EntityWitness {state:'COMPLETE'}) CALL (w) {
      MATCH (e:Entity) WHERE e.id = w.entity_id
      SET e.witness_generation=w.generation,e.witness_policy_revision=w.policy_revision
    } IN TRANSACTIONS OF ${batch} ROWS`);
    const witnessCount = dryRun || !gatesPass ? before.EntityWitness : await number("MATCH (e:Entity) WHERE e.witness_generation IS NOT NULL RETURN count(e) AS count");
    const witnessSample = dryRun || !gatesPass ? [] : (await session.run<{ id: string }>(`MATCH (w:EntityWitness) MATCH (e:Entity) WHERE e.id = w.entity_id
      WITH w,e ORDER BY w.entity_id LIMIT 1000
      WHERE NOT coalesce(e.witness_generation = w.generation,false)
         OR NOT coalesce(e.witness_policy_revision = w.policy_revision,false)
      RETURN w.entity_id AS id`)).records.map(row => row.get("id"));
    if (witnessCount !== before.EntityWitness || witnessSample.length) {
      line("copy_mismatch", { label: "EntityWitness", count: witnessCount, expected: before.EntityWitness, ids: witnessSample });
      throw new Error("copy_mismatch: Entity witness count differs");
    }
    line("copy_witness", { count: witnessCount, mismatch: 0, sampled: dryRun ? 0 : Math.min(before.EntityWitness, 1000), dry_run: dryRun });

    line("check_coverage", { coverage, split, dry_run: dryRun });
    if (split.length) throw new Error(`Extraction coverage partitions disagree: ${JSON.stringify(split)}`);
    line("check_inflight", { ...pending, dry_run: dryRun });
    if (pending.count) throw new Error("In-flight materialization work remains");
    line("fact_duplicates", { duplicates, count: duplicateCount, dry_run: dryRun });
    if (duplicateCount) throw new Error("fact_duplicates: deduplicate Facts before creating fact_identity");
    const created = ["episode_origin_head", "entity_witness", "fact_identity"];
    if (dryRun) {
      line("drop_schema", { ...legacySchema, dry_run: true });
      line("create_schema", { names: created, dry_run: true });
      line("delete_legacy_ledgers", { counts: before, batch, dry_run: true });
      line("verify", { counts: before, lineage: lineageCount, witness: witnessCount, facts, dry_run: true });
      return { counts_before: before, counts_after: before, dry_run: true, coverage };
    }
    for (const constraint of legacySchema.constraints) await session.run(`DROP CONSTRAINT \`${constraint.name}\` IF EXISTS`);
    for (const index of legacySchema.indexes) if (!index.owningConstraint) await session.run(`DROP INDEX \`${index.name}\` IF EXISTS`);
    line("drop_schema", { constraints: legacySchema.constraints.map(c => c.name), indexes: legacySchema.indexes.map(i => i.name), dry_run: false });
    await session.run("CREATE INDEX episode_origin_head IF NOT EXISTS FOR (e:Episode) ON (e.origin_key,e.ingest_seq)");
    await session.run("CREATE INDEX entity_witness IF NOT EXISTS FOR (e:Entity) ON (e.witness_generation,e.witness_policy_revision)");
    await session.run("CREATE CONSTRAINT fact_identity IF NOT EXISTS FOR (f:Fact) REQUIRE (f.generation,f.meaning_digest,f.primary_episode_id) IS UNIQUE");
    line("create_schema", { names: created, dry_run: false });
    for (const label of LABELS) {
      await session.run(`MATCH (n:${label}) CALL (n) { DETACH DELETE n } IN TRANSACTIONS OF ${batch} ROWS`);
      line("delete_legacy_ledgers", { label, count: before[label], batch, dry_run: false });
    }
    const after = await counts(), remaining = await schema();
    const newIndexes = (await session.run<{ name: string }>("SHOW INDEXES YIELD name RETURN name")).records.map(row => row.get("name"));
    const newConstraints = (await session.run<{ name: string }>("SHOW CONSTRAINTS YIELD name RETURN name")).records.map(row => row.get("name"));
    const actualLineage = await number("MATCH (e:Episode) WHERE e.lineage_mode IS NOT NULL RETURN count(e) AS count");
    const actualWitness = await number("MATCH (e:Entity) WHERE e.witness_generation IS NOT NULL RETURN count(e) AS count");
    const actualFacts = await number("MATCH (f:Fact) RETURN count(f) AS count");
    line("verify", { counts: after, constraints: remaining.constraints, indexes: remaining.indexes, new_indexes: newIndexes.filter(name => created.includes(name)),
      new_constraints: newConstraints.filter(name => created.includes(name)), lineage: actualLineage, witness: actualWitness, facts: actualFacts, dry_run: false });
    if (LABELS.some(label => after[label] !== 0) || remaining.constraints.length || remaining.indexes.length
      || actualLineage !== before.EchoLineage || actualWitness !== before.EntityWitness || actualFacts !== facts
      || !newIndexes.includes("episode_origin_head") || !newIndexes.includes("entity_witness") || !newConstraints.includes("fact_identity"))
      throw new Error("G4 verification failed");
    return { counts_before: before, counts_after: after, dry_run: false, coverage };
  } finally {
    await session.close();
    await driver.close();
  }
}

if (import.meta.main) {
  if (process.argv.slice(2).includes("--help")) console.log(USAGE);
  else {
    // no-excuse-ok: catch
    main().catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exitCode = 1;
    });
  }
}
