import { expect, test } from "bun:test";
import { mkdir, mkdtemp, writeFile } from "node:fs/promises";
import { join, resolve } from "node:path";
import { startProcess } from "./runtime-scenarios.ts";

for (const surface of ["runtime-page", "runtime-fair"] as const) {
  test(`${surface} verifies Episode revision CAS on owned Neo4j`, async () => {
    if (!process.env["ANAMNESIS_TEST_NEO4J_URI"] || !process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"])
      throw new Error("owned runner credentials required");
    const parent = resolve(".omo/evidence/foundation/g4-origin-surfaces");
    await mkdir(parent, { recursive: true });
    const root = await mkdtemp(join(parent, `${surface}-`));
    const sources: readonly (readonly [string, string])[] = [
      ["app/anamnesis/main.ts", "page-daemon.mjs"],
      ["app/anamnesis/client.ts", "page-client.mjs"],
      ...(surface === "runtime-fair" ? [["app/anamnesis/runtime-fair-real.fixture.mjs", "fair-real-fixture.mjs"] as const] : []),
    ];
    for (const [source, name] of sources) {
      const build = await startProcess(process.execPath, ["build", source, "--target=node", "--outfile", join(root, name)], { deadlineMs: 120000 }).done;
      await writeFile(join(root, `${name}.build.json`), JSON.stringify(build, null, 2));
      expect(build.code).toBe(0);
    }
    const command = ["node", `app/anamnesis/${surface}.surface.mjs`];
    await writeFile(join(root, "command.json"), JSON.stringify({ command, cwd: process.cwd() }, null, 2));
    const result = await startProcess("node", command.slice(1), {
      deadlineMs: 300000,
      env: { ...process.env, RUNTIME_PAGE_BUNDLE_ROOT: root, RUNTIME_PAGE_EVIDENCE_ROOT: join(root, "owned-db"),
        RUNTIME_FAIR_BUNDLE_ROOT: root, RUNTIME_FAIR_EVIDENCE_ROOT: join(root, "owned-db") },
      onOutput: text => process.stdout.write(text),
    }).done;
    await writeFile(join(root, "result.json"), JSON.stringify(result, null, 2));
    expect(result.timedOut).toBe(false);
    expect(result.code).toBe(0);
  }, 480000);
}
