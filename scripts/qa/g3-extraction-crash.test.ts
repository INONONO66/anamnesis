import { expect, test } from "bun:test";
import { resolve } from "node:path";
import { runG3ExtractionCrash } from "./g3-extraction-crash.ts";

test("daemon resumes a leased extraction after SIGKILL without duplicating Facts", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"];
  const password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("owned runner credentials required");
  const result = await runG3ExtractionCrash(uri, password, resolve(".omo/evidence/foundation/g3-crash"));
  expect(result).toMatchObject({ episodes: 4, facts: 4, distinct_digests: 4, covered_after_kill: 2,
    covered_final: 4, leased_line_seen: true, settled_line_seen: true });
  // The driver bounds every wait itself (database readiness 180 s, each daemon event 90 s). The restarted daemon settles the
  // lost lease only at its expiry (LEASE_MS 90 s), after the bundle build and two full pipeline runs: a pass took 184 s.
}, 480_000);
