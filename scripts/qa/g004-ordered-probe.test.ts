import { test, expect } from "bun:test";
import { mkdir, writeFile } from "node:fs/promises";
import { resolve } from "node:path";
import { startProcess } from "./runtime-scenarios.ts";

test("owned production ordered probe and Episode envelope", async () => {
  const root = resolve(process.env["G004_ORDERED_OUTPUT"] ?? ".omo/evidence/g004-ordered-probe-fix/current");
  await mkdir(root, { recursive: true });
  for (const [source, name] of [["scripts/qa/g004-ordered-probe.fixture.mjs", "fixture.mjs"], ["app/anamnesis/main.ts", "daemon.mjs"]]) {
    const build = await startProcess(process.execPath, ["build", source!, "--target=node", "--outfile", `${root}/${name}`], { deadlineMs: 120000 }).done;
    await writeFile(`${root}/${name}.build.json`, JSON.stringify(build, null, 2));
    expect(build.code).toBe(0);
  }
  const result = await startProcess("node", ["--test", `${root}/fixture.mjs`], { deadlineMs: 120000, env: { ...process.env, G004_ORDERED_OUTPUT: root }, onOutput: text => process.stdout.write(text) }).done;
  await writeFile(`${root}/node-result.json`, JSON.stringify(result, null, 2));
  expect(result.timedOut).toBe(false);
  expect(result.code).toBe(0);
}, 300000);
