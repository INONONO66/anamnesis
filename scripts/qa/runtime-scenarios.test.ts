import { expect, test } from "bun:test";
import { rejects } from "node:assert/strict";
import { cleanupOwnedContainer, parseOptions, STARTED_LINE, startProcess, type ProcessResult } from "./runtime-scenarios.ts";

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
