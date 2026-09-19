import { test, expect } from "bun:test";
import * as runner from "./g004-gds-solver-20.runner.ts";

test("benchmark admits the default unlicensed Community runtime without Enterprise claims", () => {
  expect(runner.validateGdsEdition("Unlicensed", false)).toBe("default-community");
  expect(runner.validateGdsEdition("Community", false)).toBe("default-community");
  expect(() => runner.validateGdsEdition("Enterprise", true)).toThrow();
  expect(() => runner.validateGdsEdition("Unlicensed", true)).toThrow();
  expect(() => runner.validateGdsEdition("unknown", false)).toThrow();
});

test("benchmark requests the qualified reference convergence configuration", () => {
  expect(runner.ALGORITHM.tolerance).toBe(1e-12);
  expect(runner.ALGORITHM.maxIterations).toBe(10000);
  expect(runner.ALGORITHM.dampingFactor).toBe(0.85);
});

const cid = "a".repeat(64);
const volume = "b".repeat(64);
const owner = "test-owner";
const name = "gds-test-reserved";
const ok = (stdout = "") => ({ code: 0, signal: null, stdout, stderr: "" });
const failure = (stderr: string) => ({ code: 1, signal: null, stdout: "", stderr });

function commands(options: { foreign?: boolean; logFailure?: boolean; stopFailure?: boolean; absent?: boolean; removalFailure?: boolean; oom?: boolean } = {}) {
  const calls: string[][] = [];
  let removed = false;
  let running = true;
  return {
    calls,
    async run(argv: string[]) {
      calls.push(argv);
      switch (argv[1]) {
        case "inspect":
          return options.absent ? failure(`Error: No such object: ${name}`) : ok(JSON.stringify([{
            Id: cid, Config: { Labels: { "omo.owner": options.foreign ? "someone-else" : owner, "omo.task": "gds-solver-20" } },
            State: { Running: running, OOMKilled: options.oom ?? false, ExitCode: options.oom ? 137 : 0 },
            Mounts: [{ Type: "volume", Name: volume }],
          }]));
        case "logs": return options.logFailure ? failure("log retrieval failed") : ok();
        case "stop":
          if (options.stopFailure) return failure("stop failed");
          running = false;
          return ok(cid);
        case "rm":
          if (options.removalFailure) return failure("remove failed");
          removed = true;
          return ok(cid);
        case "container": return ok(removed ? "" : cid);
        case "volume": return ok(removed ? "" : volume);
        default: throw new Error(`Unexpected cleanup command: ${argv.join(" ")}`);
      }
    },
  };
}

test("cleanup recovers a created container by reserved name when create output was lost", async () => {
  const fake = commands();
  const result = await runner.cleanupGdsContainer(fake.run, name, owner);
  expect(fake.calls[0]).toEqual(["docker", "inspect", name]);
  expect(fake.calls).toContainEqual(["docker", "rm", "-f", "-v", cid]);
  expect(result.containerAbsent).toBe(true);
  expect(result.volumesAbsent).toBe(true);
  expect(result.errors).toEqual([]);
});

test("cleanup refuses another owner's container without mutating it", async () => {
  const fake = commands({ foreign: true });
  await expect(runner.cleanupGdsContainer(fake.run, name, owner)).rejects.toThrow("ownership");
  expect(fake.calls).toEqual([["docker", "inspect", name]]);
});

test("logging and graceful-stop failures do not skip owned container removal", async () => {
  const fake = commands({ logFailure: true, stopFailure: true });
  const result = await runner.cleanupGdsContainer(fake.run, name, owner);
  expect(fake.calls).toContainEqual(["docker", "rm", "-f", "-v", cid]);
  expect(result.containerAbsent).toBe(true);
  expect(result.volumesAbsent).toBe(true);
  expect(result.errors).toHaveLength(2);
});

test("absent reserved container needs no destructive cleanup", async () => {
  const fake = commands({ absent: true });
  const result = await runner.cleanupGdsContainer(fake.run, name, owner);
  expect(result.containerAbsent).toBe(true);
  expect(fake.calls).toHaveLength(1);
});

test("failed removal cannot be reported as successful cleanup", async () => {
  const fake = commands({ removalFailure: true });
  const result = await runner.cleanupGdsContainer(fake.run, name, owner);
  expect(result.containerAbsent).toBe(false);
  expect(result.volumesAbsent).toBe(false);
  expect(result.errors.length).toBeGreaterThan(0);
});

test("an OOM remains a failed benchmark even when resource cleanup succeeds", async () => {
  const fake = commands({ oom: true });
  const result = await runner.cleanupGdsContainer(fake.run, name, owner);
  expect(result.containerAbsent).toBe(true);
  expect(result.errors.some(error => error.includes("OOM"))).toBe(true);
  expect(fake.calls).toContainEqual(["docker", "rm", "-f", "-v", cid]);
});
