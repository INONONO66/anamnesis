import { expect, test } from "bun:test";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { createContinuousDriver } from "./g006-continuous-driver.ts";
import { runContinuousSoak, type ContinuousSoakDriver } from "./g006-runtime-scenarios.ts";
import type { startProcess } from "./runtime-scenarios.ts";

const idleDriver = (): ContinuousSoakDriver => ({
  launchOwnedDaemon: async () => {}, subscribe: async () => {}, traffic: async () => {},
  recover: async () => {}, terminal: async () => {}, cleanup: async () => {}, hashEvidence: async () => ({}),
});

test("continuous launch uses Node and an explicit owned endpoint, never the Bun runner or inherited DB", async () => {
  const evidence = await mkdtemp(join(tmpdir(), "g006-launch-regression-"));
  let captured: { command: string; options: Parameters<typeof startProcess>[2] } | undefined;
  const driver = createContinuousDriver(((command, _args, options) => {
    captured = { command, options };
    return { ready: Promise.resolve(true), done: Promise.resolve({ code: 0, signal: null, timedOut: false, output: "" }), stop() {}, pid: 123 };
  }) satisfies typeof startProcess, evidence, ".", new AbortController().signal, { uri: "bolt://127.0.0.1:17687", user: "neo4j", password: "private-test-password" });
  try {
    await driver.launchOwnedDaemon();
    expect(captured?.command).toBe("node");
    expect(captured?.options.env?.ANAMNESIS_NEO4J_URI).toBe("bolt://127.0.0.1:17687");
    expect(captured?.options.env?.ANAMNESIS_NEO4J_PASSWORD).toBe("private-test-password");
    const events = (await readFile(join(evidence, "continuous-events.jsonl"), "utf8")).trim().split("\n").map(line => JSON.parse(line));
    expect(events.find(event => event.event === "daemon_ready")).toMatchObject({
      eventSequence: expect.any(Number), monotonicMs: expect.any(Number), at: expect.any(String),
    });
  } finally { await driver.cleanup(); await rm(evidence, { recursive: true, force: true }); }
});

test("continuous failure retains the first delivery error and separate cleanup error", async () => {
  const driver = idleDriver();
  driver.traffic = async () => { throw Object.assign(new Error("delivery-lost"), { code: "outcome_unknown", retryable: false }); };
  driver.cleanup = async () => { throw new Error("shutdown-connection-closed"); };
  const records: unknown[] = [];
  const result = await runContinuousSoak({ durationMs: 100, now: () => 0, signal: new AbortController().signal,
    driver, record: async value => { records.push(structuredClone(value)); } });
  expect(result.status).toBe("UNKNOWN");
  expect(result.reason).toContain("delivery-lost");
  expect(result.cleanupError).toContain("shutdown-connection-closed");
  expect(result.completedAt).toBeUndefined();
  expect(result.trafficCount).toBe(0);
  expect(records.at(-1)).toEqual(result);
});

test("hash failure cannot leave a COMPLETE continuous result", async () => {
  const driver = idleDriver();
  driver.hashEvidence = async () => { throw new Error("artifact-read-failed"); };
  const result = await runContinuousSoak({ durationMs: 1, now: (() => { let n = 0; return () => n++; })(), signal: new AbortController().signal,
    driver, record: async () => {} });
  expect(result.status).toBe("UNKNOWN");
  expect(result.completedAt).toBeUndefined();
});

test("aborting the last admitted action cannot cross the terminal boundary", async () => {
  const controller = new AbortController();
  let now = 0, terminal = false;
  const driver = idleDriver();
  driver.traffic = async () => { now = 2; controller.abort(); };
  driver.terminal = async () => { terminal = true; };
  const result = await runContinuousSoak({ durationMs: 1, now: () => now, signal: controller.signal, driver, record: async () => {} });
  expect(result.status).toBe("UNKNOWN");
  expect(result.completedAt).toBeUndefined();
  expect(terminal).toBe(false);
});
