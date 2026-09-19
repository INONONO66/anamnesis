import { expect, test } from "bun:test";
import { mkdir, mkdtemp, readFile, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { createHash } from "node:crypto";
import { startProcess } from "./runtime-scenarios.ts";

test("G004 lifecycle runs current-built real Node, UDS and owned Neo4j", async () => {
  if (!process.env["ANAMNESIS_TEST_NEO4J_URI"] || !process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]) throw new Error("owned runner credentials required");
  const parent = resolve(process.env["G004_EVIDENCE"] ?? ".omo/evidence/g004-extraction-lifecycle");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "node-"));
  for (const [source, name] of [["app/anamnesis/main.ts", "daemon.mjs"], ["scripts/qa/g004-extraction-lifecycle.fixture.mjs", "fixture.mjs"]]) {
    const result = await startProcess(process.execPath, ["build", source!, "--target=node", "--outfile", join(root, name!)], { deadlineMs: 120000 }).done;
    await writeFile(join(root, `${name}.build.json`), JSON.stringify(result, null, 2));
    expect(result.code).toBe(0);
    await writeFile(join(root, `${name}.sha256`), createHash("sha256").update(await readFile(join(root, name!))).digest("hex") + "\n");
  }
  const command = ["--test", "--test-concurrency=1", join(root, "fixture.mjs")];
  await writeFile(join(root, "command.json"), JSON.stringify({ command: ["node", ...command], cwd: process.cwd() }, null, 2));
  const result = await startProcess("node", command, { deadlineMs: 180000,
    env: { ...process.env, G004_DAEMON: join(root, "daemon.mjs") }, onOutput: text => process.stdout.write(text) }).done;
  await writeFile(join(root, "result.json"), JSON.stringify(result, null, 2));
  expect(result.timedOut).toBe(false); expect(result.code).toBe(0);
}, 450000);
