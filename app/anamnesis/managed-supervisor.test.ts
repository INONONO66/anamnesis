import assert from "node:assert/strict";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { managed } from "./managed.ts";

test("managed restarts a crashing child with exponential backoff and a budget of three", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-managed-sup-"));
  const entry = join(root, "crash.mjs");
  await writeFile(entry, "process.exit(3);\n");
  const previous = process.env["ANAMNESIS_RUNTIME_ROOT"];
  process.env["ANAMNESIS_RUNTIME_ROOT"] = root;
  const waits: number[] = [];
  try {
    // The injected delay records instead of sleeping, so the three restarts and the exhausted budget take no wall time.
    await assert.rejects(managed(entry, async ms => { waits.push(ms); }), /managed_restart_budget_exhausted/);
  } finally {
    if (previous === undefined) delete process.env["ANAMNESIS_RUNTIME_ROOT"]; else process.env["ANAMNESIS_RUNTIME_ROOT"] = previous;
    await rm(root, { recursive: true, force: true });
  }
  assert.deepEqual(waits, [1_000, 2_000, 4_000]);
});

test("a termination signal stops the supervised child once and the supervisor returns", async () => {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-managed-sup-"));
  const entry = join(root, "serve.mjs");
  await writeFile(entry, `process.kill(process.ppid, "SIGUSR2"); setInterval(() => {}, 1000);\n`);
  const previous = process.env["ANAMNESIS_RUNTIME_ROOT"];
  process.env["ANAMNESIS_RUNTIME_ROOT"] = root;
  const before = new Set(process.listeners("SIGTERM"));
  let announced: () => void = () => {};
  let run: Promise<void> | undefined;
  try {
    // The child announces itself by signalling its parent; the listener is armed before the supervisor spawns it.
    // A filesystem watcher is deliberately not used: on some macOS hosts a non-recursive directory watch delivers no events at all.
    const up = new Promise<void>((resolve, reject) => {
      announced = resolve;
      AbortSignal.timeout(4000).addEventListener("abort", () => reject(new Error("the child never announced itself")));
    });
    // Stays armed (not `once`): a second signal from a restarted child must never reach the default handler.
    process.on("SIGUSR2", announced);
    run = managed(entry, async () => { throw new Error("a stopped child must not be restarted"); });
    await up;
    const stop = process.listeners("SIGTERM").find(listener => !before.has(listener));
    assert.ok(stop);
    stop("SIGTERM");
    stop("SIGTERM");
    await run;
    assert.equal(process.listeners("SIGTERM").includes(stop), false);
  } finally {
    // On a failed path the supervisor is still running: stop it through its listener so no child outlives the test.
    for (const listener of process.listeners("SIGTERM")) if (!before.has(listener)) listener("SIGTERM");
    await run?.catch(() => undefined);
    // Only after the child is gone: a late SIGUSR2 with no listener would kill the whole test process.
    process.off("SIGUSR2", announced);
    if (previous === undefined) delete process.env["ANAMNESIS_RUNTIME_ROOT"]; else process.env["ANAMNESIS_RUNTIME_ROOT"] = previous;
    await rm(root, { recursive: true, force: true });
  }
});
