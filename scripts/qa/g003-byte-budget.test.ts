import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { mkdtemp, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { startProcess } from "./runtime-scenarios.ts";

async function currentBundle() {
  const override = process.env.RPC_BYTE_BUNDLE_ROOT;
  if (override) return { env: {}, cleanup: async () => {} };
  const directory = await mkdtemp(join(tmpdir(), "g003-byte-bundle-"));
  const output = join(directory, "byte-fixture.mjs");
  const build = startProcess(process.execPath, ["build", "app/anamnesis/rpc-byte-budget.fixture.mjs", "--target=node", "--outfile", output], { deadlineMs: 120000 });
  const result = await build.done;
  if (result.code !== 0 || result.timedOut) throw new Error(`fixture build failed: ${JSON.stringify(result)}`);
  const sourceHash = createHash("sha256").update(await readFile("app/anamnesis/rpc-byte-budget.fixture.mjs")).digest("hex");
  const bundleHash = createHash("sha256").update(await readFile(output)).digest("hex");
  console.log(JSON.stringify({ fixture_source: "app/anamnesis/rpc-byte-budget.fixture.mjs", fixture_bundle: output, source_sha256: sourceHash, bundle_sha256: bundleHash }));
  return { env: { RPC_BYTE_BUNDLE_ROOT: directory }, cleanup: () => rm(directory, { recursive: true, force: true }) };
}

test("G003 byte-budget backpressure on real Node UDS", async () => {
  const bundle = await currentBundle();
  try {
    const child = startProcess("node", ["--test", "app/anamnesis/rpc-byte-budget.test.mjs"], {
      deadlineMs: 240000, env: { ...process.env, ...bundle.env }, onOutput: text => process.stdout.write(text),
    });
    const result = await child.done;
    expect(result.timedOut).toBe(false);
    expect(result.code).toBe(0);
  } finally { await bundle.cleanup(); }
}, 380000);
