import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { startProcess } from "./runtime-scenarios.ts";

async function currentBundle(source: string, name: string, variable: string) {
  const override = process.env[variable];
  if (override) return { env: {}, cleanup: async () => {} };
  const directory = await mkdtemp(join(tmpdir(), "g003-publication-bundle-"));
  const output = join(directory, name);
  const build = startProcess(process.execPath, ["build", source, "--target=node", "--outfile", output], { deadlineMs: 120000 });
  const result = await build.done;
  if (result.code !== 0 || result.timedOut) throw new Error(`fixture build failed: ${JSON.stringify(result)}`);
  const sourceHash = createHash("sha256").update(await readFile(source)).digest("hex");
  const bundleHash = createHash("sha256").update(await readFile(output)).digest("hex");
  console.log(JSON.stringify({ fixture_source: source, fixture_bundle: output, source_sha256: sourceHash, bundle_sha256: bundleHash }));
  return { env: { [variable]: output }, cleanup: () => rm(directory, { recursive: true, force: true }) };
}

test("G003 publication on real Node UDS and owned Neo4j", async () => {
  const bundle = await currentBundle("app/anamnesis/g003-publication.fixture.mjs", "publication-fixture.mjs", "G003_PUBLICATION_BUNDLE");
  try {
    const child = startProcess("node", ["--test", "app/anamnesis/g003-publication.test.mjs"], {
      deadlineMs: 240000, env: { ...process.env, ...bundle.env }, onOutput: text => process.stdout.write(text),
    });
    const result = await child.done;
    expect(result.timedOut).toBe(false);
    expect(result.code).toBe(0);
  } finally { await bundle.cleanup(); }
}, 380000);
