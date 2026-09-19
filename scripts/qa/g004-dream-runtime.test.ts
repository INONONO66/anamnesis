import { expect, test } from "bun:test";
import { mkdtemp, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { startProcess } from "./runtime-scenarios.ts";

test("dream RPC real Node/UDS/owned Neo4j lifecycle", async () => {
  expect(process.env.ANAMNESIS_TEST_NEO4J_URI).toBeTruthy();
  expect(process.env.ANAMNESIS_TEST_NEO4J_PASSWORD).toBeTruthy();
  const root = await mkdtemp(resolve(".omo/evidence/runtime-app/dream-"));
  const daemon = join(root, "daemon.mjs"), fixture = join(root, "fixture.mjs");
  for (const [source, out] of [["app/anamnesis/main.ts", daemon], ["scripts/qa/g004-dream-runtime.fixture.mjs", fixture]] as const) {
    const built = await startProcess(process.execPath, ["build", source, "--target=node", "--outfile", out], { deadlineMs: 120000 }).done;
    await writeFile(`${out}.build.json`, JSON.stringify(built)); expect(built.code).toBe(0);
  }
  const result = await startProcess("node", ["--test", fixture], { deadlineMs: 240000,
    env: { ...process.env, G004_DAEMON: daemon, DREAM_RUNTIME_ROOT: join(root, "runtime"), ANAMNESIS_NEO4J_URI: process.env.ANAMNESIS_TEST_NEO4J_URI, ANAMNESIS_NEO4J_USER: process.env.ANAMNESIS_TEST_NEO4J_USER ?? "neo4j", ANAMNESIS_NEO4J_PASSWORD: process.env.ANAMNESIS_TEST_NEO4J_PASSWORD }, onOutput: text => process.stdout.write(text) }).done;
  await writeFile(join(root, "result.json"), JSON.stringify(result));
  expect(result.timedOut).toBe(false); expect(result.code).toBe(0);
}, 360000);
