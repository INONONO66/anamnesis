import { describe, expect, test } from "bun:test";
import { SCHEMA_MIGRATIONS, SCHEMA_VERSION, SchemaMigrationError, planSchemaMigration, type SchemaMigration } from "./schema-migrations.ts";

const chain: SchemaMigration[] = [{ from: 0, to: 1, statements: ["A"] }, { from: 1, to: 2, statements: ["B", "C"] }];

describe("planSchemaMigration", () => {
  test("a fresh store records the target without running any step", () => {
    expect(planSchemaMigration(null, false)).toEqual({ fresh: true, steps: [], target: SCHEMA_VERSION });
  });
  test("an unversioned store with data is version 0 and walks every step in order", () => {
    const plan = planSchemaMigration(null, true, 2, chain);
    expect(plan.fresh).toBe(false);
    expect(plan.steps.map(s => `${s.from}->${s.to}`)).toEqual(["0->1", "1->2"]);
  });
  test("a current store runs nothing", () => {
    expect(planSchemaMigration(2, true, 2, chain).steps).toEqual([]);
  });
  test("a partially migrated store resumes from its recorded version", () => {
    expect(planSchemaMigration(1, true, 2, chain).steps).toEqual([chain[1]!]);
  });
  test("a store ahead of the build is refused", () => {
    expect(() => planSchemaMigration(3, true, 2, chain)).toThrow(new SchemaMigrationError("schema_ahead", "store is at schema 3, this build supports 2"));
  });
  test("a missing or backward step is a gap, never silently skipped", () => {
    expect(() => planSchemaMigration(0, true, 2, [chain[1]!])).toThrow(SchemaMigrationError);
    expect(() => planSchemaMigration(0, true, 2, [{ from: 0, to: 0, statements: [] }])).toThrow(/migration_gap/);
    expect(() => planSchemaMigration(0, true, 1, [{ from: 0, to: 2, statements: [] }])).toThrow(/migration_gap/);
  });
  test("the shipped chain reaches SCHEMA_VERSION from an unversioned store", () => {
    const plan = planSchemaMigration(null, true);
    expect(plan.steps.at(-1)?.to).toBe(SCHEMA_VERSION);
    expect(SCHEMA_MIGRATIONS.every((m, i) => i === 0 ? m.from === 0 : m.from === SCHEMA_MIGRATIONS[i - 1]!.to)).toBe(true);
  });
});
