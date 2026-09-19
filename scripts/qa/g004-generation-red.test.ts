import { expect, test } from "bun:test";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { startProcess } from "./runtime-scenarios.ts";

test("frozen pre-fix Store reproduces missing/lagging coverage and ABA request/reader defects", async () => {
  if (!process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]) throw new Error("owned runner required");
  const evidence = resolve(".omo/evidence/g004-generation-readiness/red-confirmed");
  await mkdir(evidence, { recursive: true });
  const replacements = new Map([
    [resolve("packages/core/src/store.ts"), resolve(".omo/evidence/g004-generation-readiness/red-source/store.ts")],
    [resolve("packages/protocol/src/extraction.ts"), resolve(".omo/evidence/g004-generation-readiness/red-source/extraction.ts")],
    [resolve("scripts/qa/g004-extraction-lifecycle.fixture.mjs"), resolve(".omo/evidence/g004-generation-readiness/red-source/lifecycle.fixture.mjs")],
  ]);
  const built = await Bun.build({ entrypoints: ["scripts/qa/g004-extraction-lifecycle.fixture.mjs"], target: "node", outdir: evidence, naming: "[name].mjs",
    plugins: [{ name: "frozen-pre-fix-source", setup(build) {
      build.onLoad({ filter: /\.(ts|mjs)$/ }, async ({ path }) => {
        const prior = replacements.get(path);
        if (prior) return { contents: await readFile(prior, "utf8"), loader: path.endsWith(".ts") ? "ts" : "js" };
        return undefined;
      });
    } }],
  });
  expect(built.success).toBe(true);
  const args = ["--test", "--test-concurrency=1", "--test-name-pattern=cutover rejects|selector version fences", resolve(evidence, "g004-extraction-lifecycle.fixture.mjs")];
  const result = await startProcess("node", args, { deadlineMs: 120000, onOutput: text => process.stdout.write(text) }).done;
  await writeFile(resolve(evidence, "result.json"), JSON.stringify(result, null, 2));
  await writeFile(resolve(evidence, "command.json"), JSON.stringify(["node", ...args]));
  expect(result.timedOut).toBe(false);
  expect(result.code).toBe(1);
  // Test-runner counts are machine evidence, not pinned explanatory prose.
  expect(result.output).toContain("fail 4");
}, 180000);
