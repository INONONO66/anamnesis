import { expect, test } from "bun:test";
import { rejects } from "node:assert/strict";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { cleanupOwnedContainer, discoverTestFiles, parseOptions, runTestFiles, STARTED_LINE, startProcess, type ProcessResult, type TestFileResult } from "./runtime-scenarios.ts";

const base = ["--case", "foundation", "--evidence-root", ".omo/evidence/test"];

test("rejects missing evidence rather than interpreting an option as a value", () => {
  expect(() => parseOptions(["--case", "foundation"])).toThrow();
});

test("rejects unknown options before acquiring Docker resources", () => {
  expect(() => parseOptions([...base, "--unknown", "value"])).toThrow();
});

test("selected test files replace default package and QA discovery", () => {
  expect(parseOptions([...base, "--test-path", "packages/core/src/remember-input.test.ts"]).testPaths)
    .toEqual(["./packages/core/src/remember-input.test.ts"]);
});

test("foundation defaults to packages plus QA tests without launching another harness", () => {
  expect(parseOptions(base).testPaths).toEqual(["packages", "scripts/qa"]);
});

test("live e2e requires explicit case selection and cannot become a test override", () => {
  expect(parseOptions(["--case", "e2e-real", "--evidence-root", "proof"]).testPaths).toEqual([]);
  expect(() => parseOptions(["--case", "e2e-real", "--evidence-root", "proof", "--test-path", "packages/core/src/remember-input.test.ts"])).toThrow();
});

const scenarioFiles = {
  "contract-and-cas": [
    "./packages/core/src/remember-input.test.ts",
    "./packages/core/src/storage-contract.test.ts",
    "./packages/protocol/src/rpc.test.ts",
  ],
  "topology-order": [
    "./packages/core/src/engine.test.ts",
    "./packages/core/src/storage-contract.test.ts",
  ],
  "legacy-compatibility": [
    "./packages/core/src/journal.test.ts",
    "./packages/core/src/legacy-integrity.test.ts",
    "./packages/core/src/storage-contract.test.ts",
  ],
  "episode-embedding-recovery": [
    "./packages/core/src/embedding-recall.test.ts",
    "./scripts/qa/g003-embedding-recall.test.ts",
  ],
  "exact-budget": [
    "./packages/core/src/embedding-recall.test.ts",
    "./scripts/qa/g003-tokenizer-runtime.test.ts",
  ],
  "receipt-policy-transaction": [
    "./packages/core/src/receipts.test.ts",
    "./scripts/qa/g003-policy-authority.test.ts",
    "./scripts/qa/g003-publication.test.ts",
  ],
  "publication-backpressure": [
    "./scripts/qa/g003-byte-budget.test.ts",
    "./scripts/qa/g003-publication.test.ts",
  ],
  "dynamics-replay": [
    "./scripts/qa/g003-dynamics-replay.test.ts",
  ],
  "extraction-dispositions": ["./scripts/qa/g004-derived-pipeline.test.ts"],
  "generation-model-cutover": ["./scripts/qa/g004-extraction-lifecycle.test.ts"],
  "coverage-reader-aba": ["./scripts/qa/g004-extraction-lifecycle.test.ts"],
  "envelope-access-plan": ["./scripts/qa/g004-envelope-access.test.mjs"],
  "gds-solver-20": ["./scripts/qa/g004-gds-solver-20.test.ts"],
  "derived-recall": ["./scripts/qa/g004-graph-ppr-real.test.mjs"],
  "dreaming-snapshot": ["./packages/core/src/dreaming-admission.test.ts"],
  "synthesis-authority": ["./packages/core/src/dreaming-admission.test.ts"],
  "archive-authority": ["./app/anamnesis/archive-manifest.test.mjs"],
  "restore-activation": ["./app/anamnesis/restore-authority.test.mjs"],
  "restore-source-rebind": ["./app/anamnesis/backup-operation.test.mjs"],
  "upgrade-compatibility": ["./app/anamnesis/archive-manifest.test.mjs"],
  "v01-acceptance": [
    "./scripts/qa/g003-v01-acceptance.test.ts",
  ],
  "calibration-and-scale": ["./scripts/qa/g005-calibration-retention.test.ts"],
  "retention-aged": ["./scripts/qa/g005-calibration-retention.test.ts"],
};

for (const [caseName, expected] of Object.entries(scenarioFiles)) {
  test(`${caseName} dispatches concrete existing files through sequential execution`, async () => {
    const options = parseOptions(["--case", caseName, "--evidence-root", "proof"]);
    expect(options.caseName).toBe(caseName);
    expect(options.testPaths.map((file) => file.startsWith("./") ? file : `./${file}`).sort()).toEqual(expected);
    const files = await discoverTestFiles(options.testPaths);
    expect(files).toEqual(expected);
    for (const file of files) expect(readFileSync(new URL(`../../${file}`, import.meta.url)).length).toBeGreaterThan(0);
    const events: string[] = [];
    const completed = await runTestFiles(files, {
      start: (file) => {
        events.push(file);
        return { done: Promise.resolve(result(file)) };
      },
      cleanup: async () => { events.push("cleanup"); },
      record: () => {},
    });
    expect(events).toEqual(expected.flatMap((file) => [file, "cleanup"]));
    expect(completed.map(({ file, code, cleanup }) => ({ file, code, cleanup })))
      .toEqual(expected.map((file) => ({ file, code: 0, cleanup: "database cleared" })));
    const override = parseOptions(["--case", caseName, "--evidence-root", "proof", "--test-path", expected[0]!]);
    expect(override.testPaths).toEqual([expected[0]!]);
    expect(await discoverTestFiles(override.testPaths)).toEqual([expected[0]!]);
    for (const directory of ["packages", "scripts/qa"]) {
      expect(() => parseOptions(["--case", caseName, "--evidence-root", "proof", "--test-path", directory])).toThrow();
    }
  });
}

test("rejects missing, malformed, duplicate and prototype case names", () => {
  for (const caseName of ["", "UNKNOWN", "Foundation", "foundation ", "constructor", "toString", "__proto__"]) {
    expect(() => parseOptions(["--case", caseName, "--evidence-root", "proof"])).toThrow();
  }
  expect(() => parseOptions(["--evidence-root", "proof"])).toThrow();
  expect(() => parseOptions([...base, "--case", "topology-order"])).toThrow();
});

test("discovers files rather than passing directories to a concurrent Bun suite", async () => {
  const files = await discoverTestFiles(["packages/core/src"]);
  expect(files).toContain("./packages/core/src/engine.test.ts");
  expect(files.every((file) => /\.(test|spec)\./.test(file))).toBe(true);
});

test("validates case, values, duplicates and file boundaries", () => {
  for (const args of [
    [], ["--case", "unknown", "--evidence-root", "proof"],
    [...base, "--evidence-root", "another"], [...base, "--test-path"],
    [...base, "--test-path", "--case"], [...base, "--test-path", "../../outside.test.ts"],
    [...base, "--test-path", "scripts/qa/runtime-scenarios.ts"],
    [...base, "--test-path", "packages/core/src/missing.test.ts"],
  ]) expect(() => parseOptions(args)).toThrow();
});

test("accepts multiple selected files", () => {
  expect(parseOptions([...base, "--test-path", "packages/core/src/remember-input.test.ts", "--test-path", "scripts/qa/runtime-scenarios.test.ts"]).testPaths)
    .toEqual(["./packages/core/src/remember-input.test.ts", "./scripts/qa/runtime-scenarios.test.ts"]);
});

const started = "2026-09-09 01:02:03.456+0000 INFO  Started.";

test("readiness is an exact complete Started log record, including fragmented output", async () => {
  const child = startProcess(process.execPath, ["-e", `
    process.stdout.write(${JSON.stringify(started.slice(0, 24))});
    process.stdout.write(${JSON.stringify(started.slice(24) + "\n")});
  `], { deadlineMs: 2000, readyLine: STARTED_LINE });
  expect(await child.ready).toBe(true);
  const result = await child.done;
  expect(result.code).toBe(0);
  expect(result.output).toBe(started + "\n");
  expect(result.timedOut).toBe(false);
});

test("rejects Started substrings and exits before readiness without waiting for deadline", async () => {
  const child = startProcess(process.execPath, ["-e", `console.log(${JSON.stringify(started + " not actually ready")}); process.exit(7);`],
    { deadlineMs: 2000, readyLine: STARTED_LINE });
  expect(await child.ready).toBe(false);
  const result = await child.done;
  expect(result.code).toBe(7);
  expect(result.timedOut).toBe(false);
});

test("spawn error settles readiness and terminal state", async () => {
  const child = startProcess("/not-an-anamnesis-executable", [], { deadlineMs: 2000, readyLine: STARTED_LINE });
  expect(await child.ready).toBe(false);
  const result = await child.done;
  expect(result.code).not.toBe(0);
  expect(result.error).toBeDefined();
  expect(result.timedOut).toBe(false);
});

test("wall deadline kills an owned child and resolves only after its terminal event", async () => {
  // Time is the behavior under test. No readiness depends on a scheduling delay.
  const child = startProcess(process.execPath, ["-e", 'const server = Bun.serve({port: 0, fetch: () => new Response("held")}); await new Promise(() => {});'],
    { deadlineMs: 20 });
  const result = await child.done;
  expect(result.timedOut).toBe(true);
  expect(result.signal).toBe("SIGKILL");
  expect(result.code).not.toBe(0);
  expect(child.pid).toBeDefined();
  expect(() => process.kill(child.pid!, 0)).toThrow();
});

test("abort after observed readiness terminates and awaits the child", async () => {
  const controller = new AbortController();
  const child = startProcess(process.execPath, ["-e", `
    const server = Bun.serve({port: 0, fetch: () => new Response("held")});
    console.log(${JSON.stringify(started)});
    await new Promise(() => {});
  `], { deadlineMs: 2000, readyLine: STARTED_LINE, signal: controller.signal });
  expect(await child.ready).toBe(true);
  controller.abort();
  const result = await child.done;
  expect(result.signal).toBe("SIGKILL");
  expect(result.timedOut).toBe(false);
  expect(() => process.kill(child.pid!, 0)).toThrow();
});

test("passes a child-only environment and preserves raw stdout and stderr", async () => {
  const previous = process.env["ANAMNESIS_QA_CHILD_ONLY"];
  const child = startProcess(process.execPath, ["-e", 'console.log(process.env.ANAMNESIS_QA_CHILD_ONLY); console.error("stderr-sentinel");'],
    { deadlineMs: 2000, env: { ...process.env, ANAMNESIS_QA_CHILD_ONLY: "child-sentinel" } });
  const result = await child.done;
  expect(result.code).toBe(0);
  expect(result.output).toContain("child-sentinel\n");
  expect(result.output).toContain("stderr-sentinel\n");
  expect(process.env["ANAMNESIS_QA_CHILD_ONLY"]).toBe(previous);
});

const id = "a".repeat(64);
const result = (output: string, code = 0): ProcessResult => ({ code, output, signal: null, timedOut: false });

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((settle) => { resolve = settle; });
  return { promise, resolve };
}

async function withRecordedResults(check: (
  record: (results: TestFileResult[]) => void,
  read: () => TestFileResult[],
) => Promise<void>) {
  const directory = mkdtempSync(join(tmpdir(), "qa-orchestration-"));
  const path = join(directory, "child-results.json");
  writeFileSync(path, "[]");
  try {
    await check(
      (results) => writeFileSync(path, JSON.stringify(results)),
      () => JSON.parse(readFileSync(path, "utf8")) as TestFileResult[],
    );
  } finally { rmSync(directory, { recursive: true }); }
}

test("orchestration waits for terminal event, database query and driver close before the next child", async () => {
  await withRecordedResults(async (record, read) => {
    // All signals are subscribed before triggering the corresponding action.
    // Bun's test deadline bounds these awaits; no scheduling delays are used.
    const terminal = deferred<ProcessResult>();
    const cleanupStarted = deferred<void>();
    const queryDone = deferred<void>();
    const closeStarted = deferred<void>();
    const closeDone = deferred<void>();
    const events: string[] = [];
    let cleanups = 0;
    const running = runTestFiles(["first", "second"], {
      start: (file) => {
        events.push(`start:${file}`);
        if (file === "second") expect(events).toEqual([
          "start:first", "terminal:first", "cleanup:first", "query:first", "closed:first", "start:second",
        ]);
        return { done: file === "first" ? terminal.promise : Promise.resolve(result("second output")) };
      },
      cleanup: async () => {
        if (++cleanups !== 1) return;
        expect(events).toEqual(["start:first", "terminal:first", "cleanup:first"]);
        cleanupStarted.resolve();
        await queryDone.promise;
        events.push("query:first");
        closeStarted.resolve();
        await closeDone.promise;
        events.push("closed:first");
      },
      record,
    });
    // Observe errors immediately, including assertion failures inside callbacks.
    const outcome = running.then(() => null, (error: unknown) => error);
    try {
      expect(events).toEqual(["start:first"]);
      expect(read()).toEqual([]);
      events.push("terminal:first", "cleanup:first");
      terminal.resolve(result("first output"));
      await Promise.race([cleanupStarted.promise, outcome]);
      expect(events).toEqual(["start:first", "terminal:first", "cleanup:first"]);
      queryDone.resolve();
      await Promise.race([closeStarted.promise, outcome]);
      expect(events).toEqual(["start:first", "terminal:first", "cleanup:first", "query:first"]);
      closeDone.resolve();
      expect(await outcome).toBeNull();
      expect(read()).toEqual([
        { ...result("first output"), file: "first", cleanup: "database cleared" },
        { ...result("second output"), file: "second", cleanup: "database cleared" },
      ]);
    } finally {
      terminal.resolve(result("first output"));
      queryDone.resolve();
      closeDone.resolve();
      await outcome;
    }
  });
}, 2000);

test("orchestration persists the completed result while cleanup is still pending", async () => {
  await withRecordedResults(async (record, read) => {
    const cleanupStarted = deferred<void>();
    const cleanupDone = deferred<void>();
    const completed = result("completed stdout and stderr");
    const running = runTestFiles(["first"], {
      start: () => ({ done: Promise.resolve(completed) }),
      cleanup: () => { cleanupStarted.resolve(); return cleanupDone.promise; },
      record,
    });
    try {
      await cleanupStarted.promise;
      expect(read()).toEqual([{ ...completed, file: "first", cleanup: "not attempted" }]);
    } finally { cleanupDone.resolve(); await running; }
  });
}, 2000);

const failedChildren: ProcessResult[] = [
  { ...result("nonzero stdout and stderr", 7), error: "child failure" },
  { ...result("timeout stdout and stderr", 0), signal: "SIGKILL", timedOut: true },
];

for (const failed of failedChildren) {
  test(`orchestration stops later children and persists cleanup after exit=${failed.code}, timedOut=${failed.timedOut}`, async () => {
    await withRecordedResults(async (record, read) => {
      const starts: string[] = [];
      let cleanups = 0;
      await rejects(runTestFiles(["before", "failed", "later"], {
        start: (file) => {
          starts.push(file);
          return { done: Promise.resolve(file === "before" ? result("before output") : failed) };
        },
        cleanup: async () => { cleanups++; },
        record,
      }));
      expect(starts).toEqual(["before", "failed"]);
      expect(cleanups).toBe(2);
      expect(read()).toEqual([
        { ...result("before output"), file: "before", cleanup: "database cleared" },
        { ...failed, file: "failed", cleanup: "database cleared" },
      ]);
    });
  }, 2000);
}

for (const completed of [result("successful child"), ...failedChildren]) {
  test(`orchestration preserves prior and completed results when cleanup rejects after exit=${completed.code}, timedOut=${completed.timedOut}`, async () => {
    await withRecordedResults(async (record, read) => {
      const starts: string[] = [];
      let cleanups = 0;
      const cleanupError = new Error("cleanup-rejection-sentinel");
      const outcome = await runTestFiles(["before", "completed", "later"], {
        start: (file) => {
          starts.push(file);
          return { done: Promise.resolve(file === "before" ? result("before output") : completed) };
        },
        cleanup: async () => { if (++cleanups === 2) throw cleanupError; },
        record,
      }).then(() => null, (error: unknown) => error);
      expect(starts).toEqual(["before", "completed"]);
      expect(outcome).toBeInstanceOf(Error);
      expect(String(outcome)).toContain(cleanupError.message);
      if (completed.code !== 0 || completed.timedOut) {
        expect(String(outcome)).toContain(`exit=${completed.code}, timedOut=${completed.timedOut}`);
      }
      expect(read()).toEqual([
        { ...result("before output"), file: "before", cleanup: "database cleared" },
        { ...completed, file: "completed", cleanup: `FAILED: ${String(cleanupError)}` },
      ]);
    });
  }, 2000);
}

test("cleanup resolves owned name without a create receipt and removes exact immutable ID and volumes", async () => {
  const calls: string[][] = [];
  let exists = true;
  const cleanup = await cleanupOwnedContainer("owned-name", "owner-token", async (args) => {
    calls.push(args);
    if (args[0] === "inspect") return result(JSON.stringify({ Id: id, Config: { Labels: { "anamnesis.qa.owner": "owner-token" } } }));
    expect(exists).toBe(true);
    expect(args).toEqual(["rm", "-f", "-v", id]);
    exists = false;
    return result(id);
  });
  expect(exists).toBe(false);
  expect(calls[0]?.at(-1)).toBe("owned-name");
  expect(calls).toHaveLength(2);
  expect(cleanup).toContain(id);
});

test("cleanup never deletes a container with different ownership", async () => {
  const calls: string[][] = [];
  await rejects(cleanupOwnedContainer("occupied-name", "owner-token", async (args) => {
    calls.push(args);
    return result(JSON.stringify({ Id: id, Config: { Labels: { "anamnesis.qa.owner": "someone-else" } } }));
  }));
  expect(calls).toHaveLength(1);
  expect(calls[0]?.[0]).toBe("inspect");
});

test("cleanup distinguishes absent resources from Docker failure and removal failure", async () => {
  expect(await cleanupOwnedContainer("absent-name", "owner", async () => result("error: no such object: absent-name", 1))).toBe("absent");
  await rejects(cleanupOwnedContainer("name", "owner", async () => result("Cannot connect to the Docker daemon", 1)));
  await rejects(cleanupOwnedContainer("name", "owner", async (args) => args[0] === "inspect"
    ? result(JSON.stringify({ Id: id, Config: { Labels: { "anamnesis.qa.owner": "owner" } } }))
    : result("removal denied", 1)));
});

for (const caseName of ["uds-ingest", "object-spool-crashes", "outage-drain-50", "source-resume", "managed-ingest-restart", "normalized-agentlog"]) {
  test(`${caseName} selects the actual Node surface and cannot be replaced with a unit test`, () => {
    const args = ["--case", caseName, "--evidence-root", "proof"];
    expect(parseOptions(args).testPaths).toEqual([]);
    expect(() => parseOptions([...args, "--test-path", "scripts/qa/runtime-scenarios.test.ts"])).toThrow();
  });
}
