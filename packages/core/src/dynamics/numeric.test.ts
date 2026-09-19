import { expect, test } from "bun:test";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { numericFixture } from "./numeric.fixture.ts";
import { createHash } from "node:crypto";
import { readFile } from "node:fs/promises";
import { ADOPTION_NUMERIC_VERSION, adoptionGain } from "./adoption-numeric.ts";
import { adopt } from "./retention.ts";

test("producer and replay are bit-identical across Bun and Node on fixed single/mixed Hit histories", async () => {
  const fixture = fileURLToPath(new URL("./numeric.fixture.ts", import.meta.url));
  const child = spawn("node", [fixture], { stdio: ["ignore", "pipe", "pipe"] });
  const chunks: Buffer[] = [], errors: Buffer[] = [];
  child.stdout.on("data", (chunk: Buffer) => chunks.push(chunk));
  child.stderr.on("data", (chunk: Buffer) => errors.push(chunk));
  const code = await new Promise<number | null>((resolve, reject) => {
    const timer = setTimeout(() => { child.kill("SIGKILL"); reject(new Error("numeric child deadline")); }, 10000);
    child.once("error", error => { clearTimeout(timer); reject(error); });
    child.once("close", value => { clearTimeout(timer); resolve(value); });
  });
  expect({ code, stderr: Buffer.concat(errors).toString() }).toEqual({ code: 0, stderr: "" });
  const node = JSON.parse(Buffer.concat(chunks).toString()) as ReturnType<typeof numericFixture>;
  const bun = numericFixture();
  expect(node).toHaveLength(5096);
  // Utility/weights/ledger identity remain exact; never widen their comparison.
  expect(node.map(({ stability, ...rest }) => rest)).toEqual(bun.map(({ stability, ...rest }) => rest));
  const differences = bun.flatMap((value, index) => value.stability === node[index]!.stability ? []
    : [{ index, bun: value.stability, node: node[index]!.stability }]);
  console.log(JSON.stringify({ fixed_histories: bun.length, stability_differences: differences.length, first: differences.slice(0, 8) }));
  expect(differences.slice(0, 8)).toEqual([]);
  // Machine-consumed version/golden pair detects changes to operation order.
  expect({ version: ADOPTION_NUMERIC_VERSION, sha256: createHash("sha256").update(JSON.stringify(bun) + "\n").digest("hex") })
    .toEqual({ version: "binary64-adoption-v1", sha256: "56c6b90573b00be6c894fcb78fa57266c90f60b45a19c81abb2ea8b54b7715e2" });
}, 15000);

test("bounded adoption kernel follows the independent 80-digit equation oracle", async () => {
  const references: { stability: number; at: number; kappa: number; expected: number }[] =
    JSON.parse(await readFile(new URL("./adoption-reference.fixture.json", import.meta.url), "utf8"));
  expect(references).toHaveLength(90);
  let maximumRelativeError = 0;
  for (const row of references) {
    const state = { stability: row.stability, lastHit: 0, hitCount: 0 };
    const actual = adopt(state, row.at, row.kappa);
    const relativeError = Math.abs(actual.stability - row.expected) / row.expected;
    maximumRelativeError = Math.max(maximumRelativeError, relativeError);
    // docs/06's 1e-12 transcendental comparison bound is used ONLY against an
    // independent mathematical oracle, never for cache or runtime equality.
    expect(relativeError).toBeLessThanOrEqual(1e-12);
    expect(actual.stability).toBeGreaterThanOrEqual(state.stability);
    expect(actual.stability).toBeLessThanOrEqual(3650);
    expect(actual.lastHit).toBe(row.at);
    if (row.kappa === 0 || row.at === 0) expect(actual.stability).toBe(state.stability);
  }
  console.log(JSON.stringify({ decimal_reference_cases: references.length, maximum_relative_error: maximumRelativeError }));
});

test("numeric kernel rejects inputs outside its proven domain and keeps clock regression inert", () => {
  for (const [s, elapsed, kappa] of [[0, 1, 1], [3651, 1, 1], [NaN, 1, 1], [1, Infinity, 1], [1, -1, 1], [1, 1, 2]])
    expect(() => adoptionGain(s!, elapsed!, kappa!)).toThrow(RangeError);
  expect(adopt({ stability: 1.5, lastHit: 10, hitCount: 7 }, 5, 1)).toEqual({ stability: 1.5, lastHit: 10, hitCount: 7 });
});
