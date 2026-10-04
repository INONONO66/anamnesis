import neo4j from "neo4j-driver";

const USAGE = "Usage: node dist/anamnesis-migrate-g3.mjs [--dry-run] [--batch <positive integer>]";
const LABELS = [
  "ModelTask",
  "ExtractionAttempt",
  "ExtractionPipeline",
  "ExtractionJudgeInput",
  "ExtractionDisposition",
] as const;
const CONSTRAINTS = [
  "extraction_attempt_id",
  "extraction_disposition_key",
  "extraction_judge_input_id",
  "extraction_pipeline_id",
  "extraction_pipeline_judge",
  "model_task_id",
  "model_task_work_key",
] as const;

type Label = typeof LABELS[number];
type Counts = Record<Label, number>;

export type MigrationOptions = {
  readonly args?: readonly string[];
  readonly uri?: string;
  readonly user?: string;
  readonly password?: string;
  readonly database?: string;
  readonly output?: (line: string) => void;
};

export type MigrationResult = {
  readonly counts_before: Counts;
  readonly counts_after: Counts;
  readonly constraints_dropped: readonly string[];
  readonly dry_run: boolean;
};

function parseArguments(args: readonly string[]): { readonly dryRun: boolean; readonly batch: number } {
  let dryRun = false;
  let batch = 10_000;
  for (let index = 0; index < args.length; index += 1) {
    const argument = args[index];
    if (argument === "--dry-run") {
      dryRun = true;
    } else if (argument === "--batch") {
      const value = args[index + 1];
      if (!value || !/^[1-9]\d*$/.test(value)) throw new Error(`--batch must be a positive integer\n${USAGE}`);
      batch = Number(value);
      if (!Number.isSafeInteger(batch)) throw new Error(`--batch is out of bounds\n${USAGE}`);
      index += 1;
    } else {
      throw new Error(`Unknown option: ${argument}\n${USAGE}`);
    }
  }
  return { dryRun, batch };
}

function required(value: string | undefined, name: string): string {
  if (!value) throw new Error(`${name} is required to run the extraction ledger migration`);
  return value;
}

export async function main(options: MigrationOptions = {}): Promise<MigrationResult> {
  const { dryRun, batch } = parseArguments(options.args ?? process.argv.slice(2));
  const uri = required(options.uri ?? process.env["ANAMNESIS_NEO4J_URI"], "ANAMNESIS_NEO4J_URI");
  const user = required(options.user ?? process.env["ANAMNESIS_NEO4J_USER"], "ANAMNESIS_NEO4J_USER");
  const password = required(options.password ?? process.env["ANAMNESIS_NEO4J_PASSWORD"], "ANAMNESIS_NEO4J_PASSWORD");
  const database = options.database ?? process.env["ANAMNESIS_NEO4J_DATABASE"] ?? "neo4j";
  const output = options.output ?? console.log;
  const line = (step: string, fields: Record<string, unknown>) => output(JSON.stringify({ step, ...fields }));
  const driver = neo4j.driver(uri, neo4j.auth.basic(user, password), { disableLosslessIntegers: true });
  const session = driver.session({ database });

  // Schema inspection must be a standalone statement: Neo4j 5 rejects SHOW inside CALL {}.
  const schemaNames = async (): Promise<string[]> => {
    const result = await session.run<{ name: string }>(
      "SHOW CONSTRAINTS YIELD name WHERE name IN $names RETURN name",
      { names: [...CONSTRAINTS] },
    );
    return result.records.map((record) => record.get("name"));
  };
  const counts = async (): Promise<Counts> => {
    const result: Counts = {
      ModelTask: 0,
      ExtractionAttempt: 0,
      ExtractionPipeline: 0,
      ExtractionJudgeInput: 0,
      ExtractionDisposition: 0,
    };
    for (const label of LABELS) {
      const query = await session.run<{ count: number }>(`MATCH (n:${label}) RETURN count(n) AS count`);
      const value = query.records[0]?.get("count");
      if (typeof value !== "number") throw new Error(`Neo4j returned a non-numeric ${label} count`);
      result[label] = value;
    }
    return result;
  };

  try {
    const counts_before = await counts();
    const present = await schemaNames();
    line("count_legacy", { counts: counts_before, constraints: present, dry_run: dryRun });

    if (dryRun) {
      line("drop_schema", { constraints: present, dry_run: true });
      line("delete_legacy_ledgers", { counts: counts_before, batch, dry_run: true });
      line("verify", { counts: counts_before, constraints: present, dry_run: true });
      return { counts_before, counts_after: counts_before, constraints_dropped: [], dry_run: true };
    }

    for (const name of CONSTRAINTS) await session.run(`DROP CONSTRAINT ${name} IF EXISTS`);
    line("drop_schema", { constraints: present, dry_run: false });
    for (const label of LABELS) {
      await session.run(`MATCH (n:${label}) CALL (n) { DETACH DELETE n } IN TRANSACTIONS OF ${batch} ROWS`);
      line("delete_legacy_ledgers", { label, count: counts_before[label], batch, dry_run: false });
    }

    const counts_after = await counts();
    const remaining = await schemaNames();
    line("verify", { counts: counts_after, constraints: remaining, dry_run: false });
    if (LABELS.some((label) => counts_after[label] !== 0) || remaining.length > 0) {
      throw new Error(`Extraction ledger deletion incomplete: counts=${JSON.stringify(counts_after)}, constraints=${JSON.stringify(remaining)}`);
    }
    return { counts_before, counts_after, constraints_dropped: present, dry_run: false };
  } finally {
    await session.close();
    await driver.close();
  }
}

if (import.meta.main) {
  if (process.argv.slice(2).includes("--help")) {
    console.log(USAGE);
  } else {
    // no-excuse-ok: catch
    main().catch((error: unknown) => {
      console.error(error instanceof Error ? error.message : String(error));
      process.exitCode = 1;
    });
  }
}
