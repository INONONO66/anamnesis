import { expect, test } from "bun:test";
import { mkdir, mkdtemp, writeFile, readFile } from "node:fs/promises";
import { resolve, join } from "node:path";
import { createHash } from "node:crypto";
import { startProcess } from "./runtime-scenarios.ts";

test("prospective Episode lineage contract on owned Node/UDS/Neo4j", async () => {
  expect(Bun.version).toBe("1.4.1");
  if (!process.env["ANAMNESIS_TEST_NEO4J_URI"] || !process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"]) throw new Error("owned runner credentials required");
  const parent = resolve(process.env["G004_LINEAGE_EVIDENCE"] ?? ".omo/evidence/g004-episode-lineage-recovery/node");
  await mkdir(parent, { recursive: true });
  const root = await mkdtemp(join(parent, "node-"));
  for (const [source, name] of [["app/anamnesis/main.ts", "daemon.mjs"], ["scripts/qa/g004-episode-lineage.fixture.mjs", "fixture.mjs"]] as const) {
    const build = await startProcess(process.execPath, ["build", source, "--target=node", "--outfile", join(root, name)], { deadlineMs: 120000 }).done;
    await writeFile(join(root, name + ".build.json"), JSON.stringify(build, null, 2));
    expect(build.code).toBe(0);
    await writeFile(join(root, name + ".sha256"), createHash("sha256").update(await readFile(join(root, name))).digest("hex") + "\n");
  }
  const command = ["--test", "--test-concurrency=1", join(root, "fixture.mjs")];
  await writeFile(join(root, "command.json"), JSON.stringify({ cwd: process.cwd(), command: ["node", ...command] }, null, 2));
  const result = await startProcess("node", command, { deadlineMs: 180000, env: { ...process.env, G004_LINEAGE_DAEMON: process.env["G004_LINEAGE_BASELINE_DAEMON"] ?? join(root, "daemon.mjs") } }).done;
  await writeFile(join(root, "result.json"), JSON.stringify(result, null, 2));
  process.stdout.write(result.output);
  expect(result.timedOut).toBe(false);
  expect(result.code).toBe(0);
  expect(result.output).not.toMatch(/"event":"(?:recall_publication_error|runtime_error)"/);
}, 450000);
