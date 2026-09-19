import { expect, test } from "bun:test";
import { parseOptions } from "./runtime-scenarios.ts";
import { mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { executeG006, ScenarioIncompleteError, runBoundedScenarios, runContinuousSoak, type ContinuousSoakDriver } from "./g006-runtime-scenarios.ts";
import type { startProcess } from "./runtime-scenarios.ts";
import { createContinuousDriver } from "./g006-continuous-driver.ts";

// Build plumbing is isolated, but executeG006 and the continuous state machine
// both run. Interruption follows an exact traffic event, never elapsed test time.
test("continuous driver rejects missing owned password and still permits cleanup", async () => {
  const evidence = await mkdtemp(join(tmpdir(), "g006-credentials-missing-"));
  try {
    const driver = createContinuousDriver((() => { throw new Error("daemon must not launch"); }) as typeof startProcess, evidence, ".", new AbortController().signal, undefined);
    await expect(driver.launchOwnedDaemon()).rejects.toThrow("owned runner credentials required");
    await expect(driver.cleanup()).resolves.toBeUndefined();
  } finally {
    await rm(evidence, { recursive: true, force: true });
  }
});

test("continuous driver injects the owned password only into the daemon environment", async () => {
  const password = "g006-test-password";
  const evidence = await mkdtemp(join(tmpdir(), "g006-credentials-valid-"));
  let options: Parameters<typeof startProcess>[2] | undefined;
  try {
    const driver = createContinuousDriver(((_command, _args, received) => {
      options = received;
      return { done: Promise.resolve({ code: 0, signal: null, output: "", timedOut: false }), ready: Promise.resolve(true), stop() {}, pid: undefined };
    }) satisfies typeof startProcess, evidence, ".", new AbortController().signal, { uri: "bolt://127.0.0.1:17687", user: "neo4j", password });
    await driver.launchOwnedDaemon();
    expect(options?.env?.ANAMNESIS_NEO4J_PASSWORD).toBe(password);
    expect(options?.env?.ANAMNESIS_TEST_NEO4J_PASSWORD).toBe(process.env.ANAMNESIS_TEST_NEO4J_PASSWORD);
    await driver.cleanup();
  } finally {
    await rm(evidence, { recursive: true, force: true });
  }
});

test("executeG006 soak dispatches the continuous driver, not sequential case runners", async () => {
  const evidence = await mkdtemp(join(tmpdir(), "g006-dispatch-"));
  const controller = new AbortController();
  const calls: string[] = [];
  const dependencies = {
    signal: controller.signal,
    startProcess: ((_command, args) => {
      const done = writeFile(args.at(-1)!, "build-fixture").then(() => ({ code: 0, signal: null, output: "", timedOut: false }));
      return { done, ready: Promise.resolve(true), stop() {}, pid: undefined };
    }) satisfies typeof startProcess,
    runCase: async () => { calls.push("legacy"); throw new Error("legacy sequential runner reached"); },
    createContinuousDriver: (): ContinuousSoakDriver => ({
      launchOwnedDaemon: async () => { calls.push("launch"); },
      subscribe: async () => { calls.push("subscribe"); },
      traffic: async () => { calls.push("traffic"); controller.abort(); },
      recover: async () => { calls.push("recover"); },
      terminal: async () => { calls.push("terminal"); },
      hashEvidence: async () => { calls.push("hash"); return {}; },
      cleanup: async () => { calls.push("cleanup"); },
    }),
  };
  try {
    const error = await executeG006({ caseName: "soak-24h", soak: { durationMs: 86_400_000, maxIterations: 1 } }, evidence, dependencies).then(() => null, error => error);
    expect(calls).toEqual(["launch", "subscribe", "traffic", "cleanup"]);
    expect(error).toBeInstanceOf(ScenarioIncompleteError);
    const result = JSON.parse(await readFile(join(evidence, "result.json"), "utf8"));
    expect(result).toMatchObject({ status: "UNKNOWN", qualification: "UNKNOWN", reason: "interrupted", trafficCount: 1 });
    expect(result.completedAt).toBeUndefined();
    expect(JSON.parse(await readFile(join(evidence, "contract.json"), "utf8"))).toMatchObject({ targetDurationMs: 86_400_000, durationMs: 86_400_000, concurrency: 1 });
  } finally { await rm(evidence, { recursive: true, force: true }); }
});

test("bounded soak exhausts its iteration contract as UNKNOWN without waiting", async () => {
  let now = 0; const runs: string[] = [];
  const result = await runBoundedScenarios({ caseName: "soak-24h", soak: { durationMs: 86_400_000, maxIterations: 1 }, now: () => now,
    signal: new AbortController().signal, run: async (name) => { runs.push(name); now += 1; return `evidence/${name}`; }, record: async () => {} });
  expect(runs).toEqual(["uds-ingest", "managed-ingest-restart"]);
  expect(result.status).toBe("UNKNOWN");
  expect(result.qualification).toBe("UNKNOWN");
  expect(result.reason).toBe("iteration_budget_exhausted");
});

test("continuous soak keeps one daemon and subscribes before traffic", async () => {
  let now = 0; const calls: string[] = []; const records: string[] = [];
  const result = await runContinuousSoak({ durationMs: 5, now: () => now, signal: new AbortController().signal,
    driver: {
      launchOwnedDaemon: async () => { calls.push("launch"); },
      subscribe: async () => { calls.push("subscribe"); },
      traffic: async () => { calls.push("traffic"); now++; },
      recover: async () => { calls.push("recover"); },
      terminal: async () => { calls.push("terminal"); },
      cleanup: async () => { calls.push("cleanup"); },
      hashEvidence: async () => ({ "result.json": "abc" }),
    }, record: async value => { records.push(value.status); },
  });
  expect(calls.filter(call => call === "launch")).toHaveLength(1);
  expect(calls.indexOf("subscribe")).toBeLessThan(calls.indexOf("traffic"));
  expect(calls.at(-1)).toBe("cleanup");
  expect(result).toMatchObject({ status: "COMPLETE", reason: "natural_duration_completed", trafficCount: 5, hashes: { "result.json": "abc" } });
  expect(records).toContain("UNKNOWN");
});

test("continuous soak remains UNKNOWN when terminal completion is not reached", async () => {
  let now = 0; const controller = new AbortController();
  const result = await runContinuousSoak({ durationMs: 5, now: () => now, signal: controller.signal,
    driver: { launchOwnedDaemon: async () => {}, subscribe: async () => {}, traffic: async () => { controller.abort(); now++; }, recover: async () => {}, terminal: async () => { throw new Error("must not complete"); }, cleanup: async () => {}, hashEvidence: async () => ({}) }, record: async () => {} });
  expect(result.status).toBe("UNKNOWN"); expect(result.completedAt).toBeUndefined(); expect(result.reason).toBe("interrupted");
});

test("soak options have explicit bounded parsing", () => {
  expect(parseOptions(["--case", "soak-24h", "--evidence-root", "proof", "--soak-duration-ms", "10", "--soak-iterations", "2"]).soak)
    .toEqual({ durationMs: 10, maxIterations: 2 });
  expect(() => parseOptions(["--case", "soak-24h", "--evidence-root", "proof", "--soak-duration-ms", "86400001"])).toThrow();
});

for (const caseName of ["release-acceptance", "soak-24h"]) {
  test(`${caseName} is registered and cannot substitute unit tests`, () => {
    const args = ["--case", caseName, "--evidence-root", "proof"];
    expect(parseOptions(args).caseName).toBe(caseName);
    expect(parseOptions(args).testPaths).toEqual([]);
    expect(() => parseOptions([...args, "--test-path", "scripts/qa/runtime-scenarios.test.ts"])).toThrow();
  });
}
