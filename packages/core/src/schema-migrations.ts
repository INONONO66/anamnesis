/** Versioned Neo4j schema. A store written before this module carries no
 * `Meta.schema_version` and is treated as version 0; every later change to
 * the retained shape appends one migration here and bumps SCHEMA_VERSION. */
export const SCHEMA_VERSION = 1;

export interface SchemaMigration {
  readonly from: number;
  readonly to: number;
  /** Cypher run in order inside the init write transaction. */
  readonly statements: readonly string[];
}

/** 0 -> 1 only records the version: the DDL in SCHEMA_STATEMENTS is idempotent. */
export const SCHEMA_MIGRATIONS: readonly SchemaMigration[] = [{ from: 0, to: 1, statements: [] }];

export type SchemaMigrationCode = "schema_ahead" | "migration_gap";
export class SchemaMigrationError extends Error {
  constructor(readonly code: SchemaMigrationCode, detail: string) { super(`${code}: ${detail}`); this.name = "SchemaMigrationError"; }
}

export interface SchemaMigrationPlan {
  /** True when no data exists yet: the target version is written, nothing runs. */
  readonly fresh: boolean;
  readonly steps: readonly SchemaMigration[];
  readonly target: number;
}

/** Decide what init must run for a store at `recorded` (null = unversioned). */
export function planSchemaMigration(recorded: number | null, hasElements: boolean, target = SCHEMA_VERSION, migrations = SCHEMA_MIGRATIONS): SchemaMigrationPlan {
  if (recorded === null && !hasElements) return { fresh: true, steps: [], target };
  const current = recorded ?? 0;
  if (current > target) throw new SchemaMigrationError("schema_ahead", `store is at schema ${current}, this build supports ${target}`);
  const steps: SchemaMigration[] = [];
  for (let at = current; at < target;) {
    const step = migrations.find(m => m.from === at);
    if (!step || step.to <= at || step.to > target) throw new SchemaMigrationError("migration_gap", `no migration from schema ${at} toward ${target}`);
    steps.push(step); at = step.to;
  }
  return { fresh: false, steps, target };
}
