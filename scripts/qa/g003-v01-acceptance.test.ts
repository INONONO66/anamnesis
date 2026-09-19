import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdir, mkdtemp, readFile, readdir, writeFile } from "node:fs/promises";
import { resolve, join, dirname } from "node:path";
import { pathToFileURL } from "node:url";
import { startProcess } from "./runtime-scenarios.ts";

const sha = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");
async function sources() {
  const hashes: Record<string, string> = {};
  async function visit(path: string): Promise<void> {
    for (const entry of (await readdir(path, { withFileTypes: true })).sort((a, b) => a.name.localeCompare(b.name))) {
      if (entry.name === "node_modules") continue;
      const file = join(path, entry.name);
      if (entry.isDirectory()) await visit(file);
      else if (/\.(?:ts|mjs|cjs|json|md)$/.test(file)) hashes[file] = sha(await readFile(file));
    }
  }
  for (const directory of ["app/anamnesis", "packages/core/src", "packages/protocol/src", "scripts/qa", "docs"]) await visit(directory);
  for (const file of ["bun.lock", "package.json", "tsconfig.json"]) hashes[file] = sha(await readFile(file));
  return hashes;
}

test("v01 acceptance is one current-built Node/UDS/owned Neo4j narrative", async () => {
  if (!process.env["ANAMNESIS_TEST_NEO4J_URI"] || !process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]) throw new Error("owned runner credentials required");
  const parent = resolve(process.env["ANAMNESIS_QA_EVIDENCE"] ?? ".omo/evidence/g003-v01-acceptance");
  await mkdir(parent, { recursive: true });
  const evidence = await mkdtemp(join(parent, "integrated-"));
  const record = (name: string, value: unknown) => writeFile(join(evidence, name), JSON.stringify(value, null, 2) + "\n");
  const before = await sources();
  await record("source-before.json", before);
  const bundles: Record<string, { source: string; sha256: string }> = {};
  // Keep the surfaces' existing relative paths, but resolve every runtime/client
  // to this isolated current-built workspace, never the worktree's old dist/.
  await mkdir(join(evidence, "dist"), { recursive: true });
  const fixtures: Record<string, { source: string; source_sha256: string; sha256: string }> = {};
  for (const source of [
    "app/anamnesis/g003-publication.test.mjs",
    "app/anamnesis/g003-v01-acceptance.scenarios.fixture.mjs",
    "app/anamnesis/embedding-recall.surface.mjs",
    "app/anamnesis/tokenizer.surface.mjs",
    "app/anamnesis/tokenizer.fixture.cjs",
    "app/anamnesis/tokenizer-install.fixture.mjs",
    "app/anamnesis/tokenizer-vocabulary.fixture.json",
  ]) {
    const original = await readFile(source);
    let content = original;
    let name = source;
    if (source === "app/anamnesis/g003-publication.test.mjs") {
      // Substitute only the registration API, retaining every real scenario
      // body/assertion verbatim. There is no nested node:test invocation.
      const registration = "import test from 'node:test';";
      const text = original.toString();
      expect(text.split(registration)).toHaveLength(2);
      content = Buffer.from(text.replace(registration,
        "import test, { runScenarios } from './g003-v01-acceptance.scenarios.fixture.mjs';") + "\nawait runScenarios();\n");
      name = "app/anamnesis/g003-publication.surface.mjs";
    }
    await mkdir(dirname(join(evidence, name)), { recursive: true });
    await writeFile(join(evidence, name), content);
    fixtures[name] = { source, source_sha256: sha(original), sha256: sha(content) };
  }
  await record("fixtures.json", fixtures);
  // Always build from this worktree. No override or old evidence bundle is an
  // implicit dependency of the mandatory selector.
  for (const [source, name] of [
    ["app/anamnesis/main.ts", "dist/anamnesis-daemon.mjs"],
    ["app/anamnesis/client.ts", "dist/anamnesis-client.mjs"],
    ["packages/core/src/receipt-digest.ts", "dist/receipt-digest.mjs"],
    ["app/anamnesis/g003-v01-acceptance.fixture.mjs", "fixture.mjs"],
  ] as const) {
    const output = join(evidence, name);
    const build = await startProcess(process.execPath, ["build", source, "--target=node", "--outfile", output], { deadlineMs: 120000 }).done;
    await record(`${name}.build.json`, build);
    expect(build.timedOut).toBe(false); expect(build.code).toBe(0);
    bundles[name] = { source, sha256: sha(await readFile(output)) };
  }
  await record("bundles.json", bundles);
  const command = ["app/anamnesis/g003-v01-acceptance.test.mjs"];
  await record("node-command.json", { command: ["node", ...command], workspace: evidence, concurrency: 1 });
  const result = await startProcess("node", command, {
    deadlineMs: 240000,
    env: { ...process.env, G003_V01_EVIDENCE: evidence, RECEIPT_DIGEST_BUNDLE: pathToFileURL(join(evidence, "dist/receipt-digest.mjs")).href },
    onOutput: text => process.stdout.write(text),
  }).done;
  await record("node-result.json", result);
  await writeFile(join(evidence, "node-output.txt"), result.output);
  const after = await sources();
  await record("source-after.json", after);
  expect(after).toEqual(before);
  for (const [name, artifact] of Object.entries({ ...bundles, ...fixtures })) expect(sha(await readFile(join(evidence, name)))).toBe(artifact.sha256);
  expect(result.timedOut).toBe(false); expect(result.code).toBe(0);
  expect(result.output).not.toMatch(/recursively|skipping running files/);
  const checkpoints = result.output.split("\n").filter(line => line.startsWith("{")).map(line => JSON.parse(line));
  const matching = (checkpoint: string) => checkpoints.filter(item => item.checkpoint === checkpoint);
  for (const checkpoint of [
    "policy-publication", "failed-send", "auto-exposure", "denied-target", "denied-provenance",
    "partial-and-crash", "audit-failure", "shutdown-pending-publication",
    "receipt-hashes", "verified", "whole-context-exact-fit-one-below-zero",
    "1MiB-response-bound", "receipt-survives-restart", "GREEN-real-daemon", "g003-v01-surfaces",
  ]) expect(matching(checkpoint)).toHaveLength(1);
  expect(matching("publication-scenario-pass")).toHaveLength(8);
  expect(matching("publication-cleanup")).toHaveLength(8);
  expect(matching("publication-scenarios-complete")).toEqual([
    { checkpoint: "publication-scenarios-complete", passed: 8, failed: 0, skipped: 0, concurrency: 1 },
  ]);
  expect(matching("GREEN-real-daemon")[0].bundle_sha256).toBe(bundles["dist/anamnesis-daemon.mjs"]!.sha256);
  await record("acceptance.json", { publication: { passed: 8, failed: 0, skipped: 0, concurrency: 1 },
    checkpoints: checkpoints.filter(item => item.checkpoint), bundles, fixtures, sources_unchanged: true });
  console.log(JSON.stringify({ checkpoint: "v01-current-build", evidence, bundles, sources_unchanged: true }));
}, 620000);
