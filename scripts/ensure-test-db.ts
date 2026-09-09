/** Standalone DB setup cannot safely transfer container ownership or environment. */
console.error(
  "Standalone test DB setup is disabled. Run the owning test harness instead:\n" +
  "bun scripts/qa/runtime-scenarios.ts --case foundation --evidence-root .omo/evidence/phase2/runner-lifecycle\n" +
  "Optionally append --test-path packages/core/src/remember-input.test.ts.",
);
process.exitCode = 1;
