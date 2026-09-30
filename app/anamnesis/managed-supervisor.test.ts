import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { managed } from "./managed.ts";

test("managed restarts a crashing child with exponential backoff and a budget of three", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-managed-sup-"));
  const entry = join(root, "crash.mjs");
  await writeFile(entry, "process.exit(3);\n");
  const previous = process.env["ANAMNESIS_RUNTIME_ROOT"];
  process.env["ANAMNESIS_RUNTIME_ROOT"] = root;
  const waits: number[] = [];
  try {
    // The injected delay records instead of sleeping, so the three restarts and the exhausted budget take no wall time.
    await assert.rejects(managed(entry, async ms => { waits.push(ms); }), /managed_restart_budget_exhausted/);
  } finally {
    if (previous === undefined) delete process.env["ANAMNESIS_RUNTIME_ROOT"]; else process.env["ANAMNESIS_RUNTIME_ROOT"] = previous;
    await rm(root, { recursive: true, force: true });
  }
  assert.deepEqual(waits, [1_000, 2_000, 4_000]);
});
