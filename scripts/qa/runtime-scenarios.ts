import { randomBytes, randomUUID } from "node:crypto";
import { appendFileSync, statSync, writeFileSync } from "node:fs";
import { mkdir, mkdtemp, readdir } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { relative, resolve, join } from "node:path";
import { fileURLToPath } from "node:url";
import neo4j from "neo4j-driver";
import { executeG006, ScenarioIncompleteError } from "./g006-runtime-scenarios.ts";
import { createContinuousDriver } from "./g006-continuous-driver.ts";
import { timingHash, timingLog } from "../../app/anamnesis/timing.ts";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OWNER_LABEL = "anamnesis.qa.owner";
const RUNTIME_CASES = new Set(["uds-ingest", "object-spool-crashes", "outage-drain-50", "source-resume", "managed-ingest-restart", "normalized-agentlog"]);
const AGGREGATE_CASES = new Set(["release-acceptance", "soak-24h"]);
const BLOCKED_CASES = new Map<string, string>();
// Storage contracts contain real CAS, ordered-edge and legacy-format assertions.
// Keep whole files: each receives the same isolated DB, cleared before the next.
const CASE_TEST_PATHS = new Map<string, string[]>([
  ["foundation", ["packages", "scripts/qa"]],
  ["contract-and-cas", [
    "packages/core/src/remember-input.test.ts",
    "packages/core/src/storage-contract.test.ts",
    "packages/protocol/src/rpc.test.ts",
  ]],
  ["topology-order", [
    "packages/core/src/engine.test.ts",
    "packages/core/src/storage-contract.test.ts",
  ]],
  ["legacy-compatibility", [
    "packages/core/src/journal.test.ts",
    "packages/core/src/legacy-integrity.test.ts",
    "packages/core/src/storage-contract.test.ts",
  ]],
  ["episode-embedding-recovery", [
    "scripts/qa/g003-embedding-recall.test.ts",
    "packages/core/src/embedding-recall.test.ts",
  ]],
  ["exact-budget", [
    "scripts/qa/g003-tokenizer-runtime.test.ts",
    "packages/core/src/embedding-recall.test.ts",
  ]],
  ["receipt-policy-transaction", [
    "packages/core/src/receipts.test.ts",
    "scripts/qa/g003-policy-authority.test.ts",
    "scripts/qa/g003-publication.test.ts",
  ]],
  ["publication-backpressure", [
    "scripts/qa/g003-publication.test.ts",
    "scripts/qa/g003-byte-budget.test.ts",
  ]],
  ["dynamics-replay", [
    "scripts/qa/g003-dynamics-replay.test.ts",
  ]],
  // Audit subset only: this selector does not certify semantic dispositions.
  ["extraction-dispositions", ["scripts/qa/g004-derived-pipeline.test.ts"]],
  ["generation-model-cutover", ["scripts/qa/g004-extraction-lifecycle.test.ts"]],
  ["coverage-reader-aba", ["scripts/qa/g004-extraction-lifecycle.test.ts"]],
  ["envelope-access-plan", ["scripts/qa/g004-envelope-access.test.mjs"]],
  ["gds-solver-20", ["scripts/qa/g004-gds-solver-20.test.ts"]],
  ["derived-recall", ["scripts/qa/g004-graph-ppr-real.test.mjs"]],
  ["dreaming-snapshot", ["packages/core/src/dreaming-admission.test.ts"]],
  ["dream-runtime", ["scripts/qa/g004-dream-runtime.test.ts"]],
  ["synthesis-authority", ["packages/core/src/dreaming-admission.test.ts"]],
  ["archive-authority", ["app/anamnesis/archive-manifest.test.mjs"]],
  ["restore-activation", ["app/anamnesis/restore-authority.test.mjs"]],
  ["restore-source-rebind", ["app/anamnesis/backup-operation.test.mjs"]],
  ["upgrade-compatibility", ["app/anamnesis/archive-manifest.test.mjs"]],
  ["calibration-and-scale", ["scripts/qa/g005-calibration-retention.test.ts"]],
  ["retention-aged", ["scripts/qa/g005-calibration-retention.test.ts"]],
  ["g004-envelope-access", [
    "scripts/qa/g004-envelope-access.test.mjs",
  ]],
  ["v01-acceptance", [
    "scripts/qa/g003-v01-acceptance.test.ts",
  ]],
]);
export const STARTED_LINE = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}(?:[+-]\d{4})?\s+INFO\s+Started\.$/;

export function parseOptions(args: string[]) {
  const values = new Map<string, string>();
  const testPaths: string[] = [];
  const soakOptions = new Set(["--soak-duration-ms", "--soak-iterations"]);
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    const value = args[i + 1];
    if (!key || (!["--case", "--evidence-root", "--test-path"].includes(key) && !soakOptions.has(key))) {
      throw new Error(`Unknown option: ${key}`);
    }
    if (!value || value.startsWith("--")) throw new Error(`Missing value for ${key}`);
    if (key === "--test-path") {
      const path = relative(ROOT, resolve(ROOT, value));
      if (!/^(app|packages|scripts\/qa)\/.+\.(test|spec)\.[cm]?[jt]sx?$/.test(path) || path.split("/").includes("..")) {
        throw new Error(`Not an application, package or QA test file: ${value}`);
      }
      if (!statSync(resolve(ROOT, path)).isFile()) throw new Error(`Not a file: ${value}`);
      testPaths.push(`./${path}`);
    } else {
      if (values.has(key)) throw new Error(`Duplicate option: ${key}`);
      values.set(key, value);
    }
  }
  const caseName = values.get("--case");
  const defaults = caseName ? CASE_TEST_PATHS.get(caseName) : undefined;
  if (!caseName || (!defaults && !RUNTIME_CASES.has(caseName) && !AGGREGATE_CASES.has(caseName))) throw new Error(`--case must be one of: ${[...CASE_TEST_PATHS.keys(), ...RUNTIME_CASES, ...AGGREGATE_CASES].join(", ")}`);
  if ((RUNTIME_CASES.has(caseName) || AGGREGATE_CASES.has(caseName)) && testPaths.length) throw new Error("runtime cases cannot be replaced by package tests");
  for (const key of soakOptions) if (values.has(key) && !AGGREGATE_CASES.has(caseName)) throw new Error(`${key} is only valid for aggregate cases`);
  const integer = (key: string, fallback: number, max: number) => {
    const raw = values.get(key); if (raw === undefined) return fallback;
    if (!/^\d+$/.test(raw)) throw new Error(`${key} must be a positive integer`);
    const value = Number(raw); if (!Number.isSafeInteger(value) || value <= 0 || value > max) throw new Error(`${key} is out of bounds`); return value;
  };
  const soak = caseName === "soak-24h" ? { durationMs: integer("--soak-duration-ms", 86_400_000, 86_400_000), maxIterations: integer("--soak-iterations", 1000, 100_000) } : undefined;
  const evidenceRoot = values.get("--evidence-root");
  if (!evidenceRoot) throw new Error("--evidence-root is required");
  return { caseName, evidenceRoot, testPaths: testPaths.length ? testPaths : [...(defaults ?? [])], soak };
}

export async function discoverTestFiles(paths: string[]): Promise<string[]> {
  const files: string[] = [];
  const visit = async (path: string): Promise<void> => {
    const info = await readdir(resolve(ROOT, path), { withFileTypes: true });
    for (const entry of info.sort((a, b) => a.name.localeCompare(b.name))) {
      const child = join(path, entry.name);
      if (entry.isDirectory()) await visit(child);
      else if (/\.(test|spec)\.[cm]?[jt]sx?$/.test(entry.name)) files.push(`./${child}`);
    }
  };
  for (const path of paths) {
    if (/\.(test|spec)\.[cm]?[jt]sx?$/.test(path)) files.push(path.startsWith("./") ? path : `./${path}`);
    else await visit(path);
  }
  return [...new Set(files)].sort();
}

export interface ProcessResult {
  code: number;
  signal: NodeJS.Signals | null;
  output: string;
  timedOut: boolean;
  error?: string;
}
export type TestFileResult = ProcessResult & { file: string; cleanup: string };

/** start().done is the child's terminal event; cleanup includes closing its driver. */
export async function runTestFiles(files: string[], options: {
  start: (file: string) => { done: Promise<ProcessResult> };
  cleanup: () => Promise<void>;
  record: (results: TestFileResult[]) => void;
  signal?: AbortSignal;
}): Promise<TestFileResult[]> {
  const results: TestFileResult[] = [];
  for (const file of files) {
    options.signal?.throwIfAborted();
    const result = await options.start(file).done;
    const completed = { ...result, file, cleanup: "not attempted" };
    results.push(completed);
    options.record(results); // Preserve the terminal result before cleanup can fail.
    const failures: unknown[] = [];
    if (result.code !== 0 || result.timedOut) {
      failures.push(new Error(`Test file failed: ${file}, exit=${result.code}, timedOut=${result.timedOut}`));
    }
    try {
      await options.cleanup();
      completed.cleanup = "database cleared";
    } catch (error) {
      completed.cleanup = `FAILED: ${String(error)}`;
      failures.push(error);
    } finally {
      options.record(results);
    }
    if (failures.length) throw new AggregateError(failures, failures.map(String).join("; "));
  }
  return results;
}

interface ProcessOptions {
  deadlineMs: number;
  env?: NodeJS.ProcessEnv;
  signal?: AbortSignal;
  readyLine?: RegExp;
  onOutput?: (text: string) => void;
}

/** Subscribe synchronously: spawn output/events are delivered after listeners exist.
 * docker start -a itself attaches before starting the previously created container. */
export function startProcess(command: string, args: string[], options: ProcessOptions & { cwd?: string }) {
  const child = spawn(command, args, { cwd: options.cwd ?? ROOT, env: options.env ?? process.env, stdio: ["ignore", "pipe", "pipe"] });
  let output = "";
  let timedOut = false;
  let error: string | undefined;
  let closed = false;
  let markReady!: (ready: boolean) => void;
  const ready = new Promise<boolean>((resolve) => { markReady = resolve; });
  const stop = () => { if (!closed) child.kill("SIGKILL"); };
  const timer = setTimeout(() => { timedOut = true; markReady(false); stop(); }, options.deadlineMs);
  const readers = [child.stdout, child.stderr].map((stream) => {
    stream.on("data", (chunk: Buffer) => {
      const text = chunk.toString();
      output += text;
      options.onOutput?.(text);
    });
    const reader = createInterface({ input: stream, crlfDelay: Infinity });
    reader.on("line", (line) => {
      if (options.readyLine?.test(line)) {
        clearTimeout(timer);
        markReady(true);
      }
    });
    return reader;
  });
  const done = new Promise<ProcessResult>((resolve) => {
    child.on("error", (cause) => { error = cause.message; markReady(false); });
    child.on("close", (code, signal) => {
      closed = true;
      clearTimeout(timer);
      options.signal?.removeEventListener("abort", stop);
      for (const reader of readers) reader.close();
      markReady(false);
      resolve({ code: code ?? 1, signal, output, timedOut, ...(error ? { error } : {}) });
    });
  });
  options.signal?.addEventListener("abort", stop, { once: true });
  if (options.signal?.aborted) stop();
  return { ready, done, stop, pid: child.pid };
}

type DockerCommand = (args: string[]) => Promise<ProcessResult>;

/** Cleanup does not depend on receiving stdout/cidfile from docker create.
 * Resolve the reserved name, verify ownership, then remove the immutable ID. */
export async function cleanupOwnedContainer(name: string, owner: string, docker: DockerCommand) {
  const inspected = await docker(["inspect", "--format", '{{json .}}', name]);
  if (inspected.code !== 0) {
    if (!inspected.timedOut && /no such (object|container):/i.test(inspected.output)) return "absent";
    throw new Error(`Cleanup inspect failed: ${inspected.error ?? inspected.output}`);
  }
  const container = JSON.parse(inspected.output) as { Id: string; Config: { Labels: Record<string, string> } };
  if (container.Config.Labels[OWNER_LABEL] !== owner || !/^[a-f0-9]{64}$/.test(container.Id)) {
    throw new Error("Cleanup refused: container ownership mismatch");
  }
  const removed = await docker(["rm", "-f", "-v", container.Id]);
  if (removed.code !== 0) throw new Error(`Cleanup removal failed: ${removed.error ?? removed.output}`);
  return `removed ${container.Id}`;
}

export async function main(args = process.argv.slice(2)): Promise<string> {
  const options = parseOptions(args); // Reject invalid input before filesystem/Docker acquisition.
  await mkdir(options.evidenceRoot, { recursive: true });
  const evidence = await mkdtemp(resolve(options.evidenceRoot, `${options.caseName}-`));
  console.log(`Evidence: ${evidence}`);
  const blockedReason = BLOCKED_CASES.get(options.caseName);
  if (blockedReason) {
    writeFileSync(resolve(evidence, "result.json"), JSON.stringify({ status: "UNKNOWN", qualification: "UNKNOWN", reason: blockedReason }) + "\n");
    throw new ScenarioIncompleteError(`UNKNOWN: ${blockedReason}; evidence: ${evidence}`);
  }
  const owner = randomUUID();
  const name = `anamnesis-qa-${owner}`;
  const password = `qa-${randomBytes(18).toString("base64url")}`;
  const redact = (text: string) => text.replaceAll(password, "[REDACTED]");
  const append = (file: string, text: string) => appendFileSync(resolve(evidence, file), redact(text));
  const record = (file: string, value: unknown) => writeFileSync(resolve(evidence, file), redact(JSON.stringify(value, null, 2) + "\n"));
  const endpointTiming = options.soak ? timingLog(resolve(evidence, "continuous-endpoint-timing.jsonl")) : undefined;
  const testFiles = await discoverTestFiles(options.testPaths);
  if (options.caseName === "release-acceptance") {
    return executeG006({ caseName: options.caseName, ...(options.soak ? { soak: options.soak } : {}) }, evidence, {
      startProcess,
      runCase: async (caseName, caseEvidence, _workspace, signal): Promise<string> => {
        const nestedEvidence: string = await main(["--case", caseName, "--evidence-root", caseEvidence]);
        signal.throwIfAborted();
        return nestedEvidence;
      },
    });
  }
  const runtimeCase = RUNTIME_CASES.has(options.caseName);
  const command = options.soak ? ["node", join(evidence, "workspace/dist/anamnesis-daemon.mjs")] : runtimeCase ? ["node", "app/anamnesis/recovery.surface.mjs", options.caseName, evidence] : [process.execPath, "test", ...testFiles];
  record("command.json", { cwd: ROOT, runner: [process.execPath, fileURLToPath(import.meta.url), ...args], command, container: name });
  for (const file of ["child-output.txt", "docker-output.txt", "cleanup.txt", "commands.jsonl"]) append(file, "");
  const controller = new AbortController();
  const interrupt = () => controller.abort();
  process.on("SIGINT", interrupt);
  process.on("SIGTERM", interrupt);
  let attemptedCreate = false;
  let attachment: ReturnType<typeof startProcess> | undefined;
  let testChild: ReturnType<typeof startProcess> | undefined;
  let childResult: ProcessResult | undefined;
  let failure: unknown;
  let cleanup = "not acquired";
  const launch = (executable: string, argv: string[], processOptions: ProcessOptions, outputFile: string) => {
    append("commands.jsonl", JSON.stringify({ command: [executable, ...argv], deadlineMs: processOptions.deadlineMs }) + "\n");
    const isEndpoint = executable === "docker";
    if (isEndpoint) endpointTiming?.({ layer: "neo4j", event: "process_start", operation: argv[0] });
    const child = startProcess(executable, argv, { ...processOptions, onOutput: text => {
      if (isEndpoint) endpointTiming?.({ layer: "neo4j", event: "output_observed", operation: argv[0], bytes: Buffer.byteLength(text), hash: timingHash(text) });
      append(outputFile, text);
    } });
    return { ...child, done: child.done.then(result => {
      if (isEndpoint) endpointTiming?.({ layer: "neo4j", event: "process_exit", operation: argv[0], hash: timingHash(JSON.stringify({ code: result.code, signal: result.signal, timedOut: result.timedOut })) });
      return result;
    }) };
  };
  const docker = (argv: string[], cleanupCommand = false) => launch("docker", argv, {
    deadlineMs: 60_000,
    ...(cleanupCommand ? {} : { signal: controller.signal }),
    env: { ...process.env, NEO4J_AUTH: `neo4j/${password}` },
  }, cleanupCommand ? "cleanup.txt" : "docker-output.txt").done;
  try {
    controller.signal.throwIfAborted();
    attemptedCreate = true;
    const created = await docker(["create", "--name", name, "--label", `${OWNER_LABEL}=${owner}`, "-p", "127.0.0.1::7687", "-e", "NEO4J_AUTH", "-e", "NEO4J_server_memory_heap_max__size=512M", "-e", "NEO4J_server_memory_pagecache_size=256M", "neo4j:5.26-community"]);
    if (created.code !== 0) throw new Error(`docker create failed: ${created.error ?? created.output}`);
    attachment = launch("docker", ["start", "-a", name], { deadlineMs: 180_000, readyLine: STARTED_LINE, signal: controller.signal }, "docker-output.txt");
    if (!await attachment.ready) {
      const result = await attachment.done;
      throw new Error(`Neo4j did not emit Started: ${JSON.stringify(result)}`);
    }
    endpointTiming?.({ layer: "neo4j", event: "ready" });
    const port = await docker(["inspect", "-f", '{{(index (index .NetworkSettings.Ports "7687/tcp") 0).HostPort}}', name]);
    if (port.code !== 0 || !/^\d+$/.test(port.output.trim())) throw new Error("Unable to inspect mapped Bolt port");
    const uri = `bolt://127.0.0.1:${port.output.trim()}`;
    record("endpoint.json", { uri, user: "neo4j", container: name });
    controller.signal.throwIfAborted();
    const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { connectionTimeout: 10_000, connectionAcquisitionTimeout: 10_000 });
    endpointTiming?.({ layer: "neo4j", event: "connectivity_start" });
    try { await driver.verifyConnectivity(); endpointTiming?.({ layer: "neo4j", event: "connectivity_complete" }); }
    catch (error) { endpointTiming?.({ layer: "neo4j", event: "connectivity_failed", hash: timingHash(String(error)) }); throw error; }
    finally { await driver.close(); }
    record("connectivity.json", { authenticated: true, attempts: 1 });
    controller.signal.throwIfAborted();
    if (options.soak) {
      await executeG006({ caseName: options.caseName, soak: options.soak }, evidence, {
        startProcess, signal: controller.signal,
        runCase: async () => { throw new Error("continuous soak cannot dispatch nested cases"); },
        createContinuousDriver: (driverEvidence, workspace, signal) => createContinuousDriver(startProcess, driverEvidence, workspace, signal, { uri, user: "neo4j", password }),
      });
    } else {
    const childResults = await runTestFiles(runtimeCase ? ["app/anamnesis/recovery.surface.mjs"] : testFiles, {
      signal: controller.signal,
      start: (file) => {
        const bunRunner = process.env.BUN_FOUNDATION_RUNNER ?? process.execPath;
        testChild = launch(runtimeCase ? "node" : bunRunner, runtimeCase ? [file, options.caseName, evidence] : ["test", file], {
          deadlineMs: 900_000,
          signal: controller.signal,
          env: { ...process.env, ANAMNESIS_TEST_NEO4J_URI: uri, ANAMNESIS_TEST_NEO4J_USER: "neo4j", ANAMNESIS_TEST_NEO4J_PASSWORD: password, ANAMNESIS_NEO4J_PASSWORD: password },
        }, "child-output.txt");
        return testChild;
      },
      cleanup: async () => {
        const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password));
        try { await driver.executeQuery("MATCH (n) DETACH DELETE n"); }
        finally { await driver.close(); }
      },
      record: (results) => {
        childResult = results.at(-1);
        testChild = undefined;
        record("child-results.json", results);
      },
    });
    childResult = { code: childResults.every((result) => result.code === 0) ? 0 : 1, signal: null, output: "", timedOut: false };
    }
    controller.signal.throwIfAborted();
  } catch (error) {
    failure = error;
  } finally {
    testChild?.stop();
    if (testChild) childResult = await testChild.done;
    try {
      if (attemptedCreate) cleanup = await cleanupOwnedContainer(name, owner, (argv) => docker(argv, true));
    } catch (error) {
      cleanup = `FAILED: ${String(error)}`;
      failure = new AggregateError(failure ? [failure, error] : [error], [failure, error].filter(Boolean).map(String).join("; "));
    } finally {
      attachment?.stop();
      if (attachment) record("attachment.json", await attachment.done);
      append("cleanup.txt", `\ncontainer=${name}\n${cleanup}\n`);
      const exitCode = failure ? failure instanceof ScenarioIncompleteError ? 2 : 1 : 0;
      record(options.soak ? "runner-result.json" : "result.json", { child: childResult ?? null, cleanup, error: failure ? String(failure) : null, exitCode });
      record("exit.code", exitCode);
      process.off("SIGINT", interrupt);
      process.off("SIGTERM", interrupt);
    }
  }
  if (failure instanceof ScenarioIncompleteError) throw new ScenarioIncompleteError(redact(String(failure)));
  if (failure) throw new Error(redact(String(failure)));
  return evidence;
}

if (import.meta.main) {
  try { await main(); } catch (error) { console.error(String(error)); process.exitCode = error instanceof ScenarioIncompleteError ? 2 : 1; }
}
