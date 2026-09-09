import { randomBytes, randomUUID } from "node:crypto";
import { appendFileSync, statSync, writeFileSync } from "node:fs";
import { mkdir, mkdtemp } from "node:fs/promises";
import { spawn } from "node:child_process";
import { createInterface } from "node:readline";
import { relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import neo4j from "neo4j-driver";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OWNER_LABEL = "anamnesis.qa.owner";
export const STARTED_LINE = /^\d{4}-\d{2}-\d{2} \d{2}:\d{2}:\d{2}\.\d{3}(?:[+-]\d{4})?\s+INFO\s+Started\.$/;

export function parseOptions(args: string[]) {
  const values = new Map<string, string>();
  const testPaths: string[] = [];
  for (let i = 0; i < args.length; i += 2) {
    const key = args[i];
    const value = args[i + 1];
    if (!key || !["--case", "--evidence-root", "--test-path"].includes(key)) {
      throw new Error(`Unknown option: ${key}`);
    }
    if (!value || value.startsWith("--")) throw new Error(`Missing value for ${key}`);
    if (key === "--test-path") {
      const path = relative(ROOT, resolve(ROOT, value));
      if (!/^(packages|scripts\/qa)\/.+\.(test|spec)\.[cm]?[jt]sx?$/.test(path) || path.split("/").includes("..")) {
        throw new Error(`Not a package or QA test file: ${value}`);
      }
      if (!statSync(resolve(ROOT, path)).isFile()) throw new Error(`Not a file: ${value}`);
      testPaths.push(`./${path}`);
    } else {
      if (values.has(key)) throw new Error(`Duplicate option: ${key}`);
      values.set(key, value);
    }
  }
  if (values.get("--case") !== "foundation") throw new Error("--case must be foundation");
  const evidenceRoot = values.get("--evidence-root");
  if (!evidenceRoot) throw new Error("--evidence-root is required");
  return { caseName: "foundation", evidenceRoot, testPaths: testPaths.length ? testPaths : ["packages", "scripts/qa"] };
}

export interface ProcessResult {
  code: number;
  signal: NodeJS.Signals | null;
  output: string;
  timedOut: boolean;
  error?: string;
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
export function startProcess(command: string, args: string[], options: ProcessOptions) {
  const child = spawn(command, args, { cwd: ROOT, env: options.env ?? process.env, stdio: ["ignore", "pipe", "pipe"] });
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

export async function main(args = process.argv.slice(2)) {
  const options = parseOptions(args); // Reject invalid input before filesystem/Docker acquisition.
  await mkdir(options.evidenceRoot, { recursive: true });
  const evidence = await mkdtemp(resolve(options.evidenceRoot, "foundation-"));
  console.log(`Evidence: ${evidence}`);
  const owner = randomUUID();
  const name = `anamnesis-qa-${owner}`;
  const password = `qa-${randomBytes(18).toString("base64url")}`;
  const redact = (text: string) => text.replaceAll(password, "[REDACTED]");
  const append = (file: string, text: string) => appendFileSync(resolve(evidence, file), redact(text));
  const record = (file: string, value: unknown) => writeFileSync(resolve(evidence, file), redact(JSON.stringify(value, null, 2) + "\n"));
  const command = [process.execPath, "test", ...options.testPaths];
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
    return startProcess(executable, argv, { ...processOptions, onOutput: (text) => append(outputFile, text) });
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
    const port = await docker(["inspect", "-f", '{{(index (index .NetworkSettings.Ports "7687/tcp") 0).HostPort}}', name]);
    if (port.code !== 0 || !/^\d+$/.test(port.output.trim())) throw new Error("Unable to inspect mapped Bolt port");
    const uri = `bolt://127.0.0.1:${port.output.trim()}`;
    record("endpoint.json", { uri, user: "neo4j", container: name });
    controller.signal.throwIfAborted();
    const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { connectionTimeout: 10_000, connectionAcquisitionTimeout: 10_000 });
    try { await driver.verifyConnectivity(); } finally { await driver.close(); }
    record("connectivity.json", { authenticated: true, attempts: 1 });
    controller.signal.throwIfAborted();
    testChild = launch(process.execPath, ["test", ...options.testPaths], {
      deadlineMs: 900_000,
      signal: controller.signal,
      env: { ...process.env, ANAMNESIS_TEST_NEO4J_URI: uri, ANAMNESIS_TEST_NEO4J_USER: "neo4j", ANAMNESIS_TEST_NEO4J_PASSWORD: password, ANAMNESIS_NEO4J_PASSWORD: password },
    }, "child-output.txt");
    childResult = await testChild.done;
    if (childResult.code !== 0 || childResult.timedOut) throw new Error(`Test suite failed: exit=${childResult.code}, timedOut=${childResult.timedOut}`);
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
      failure = new AggregateError(failure ? [failure, error] : [error], cleanup);
    } finally {
      attachment?.stop();
      if (attachment) record("attachment.json", await attachment.done);
      append("cleanup.txt", `\ncontainer=${name}\n${cleanup}\n`);
      record("result.json", { child: childResult ?? null, cleanup, error: failure ? String(failure) : null });
      process.off("SIGINT", interrupt);
      process.off("SIGTERM", interrupt);
    }
  }
  if (failure) throw new Error(redact(String(failure)));
  return evidence;
}

if (import.meta.main) {
  try { await main(); } catch (error) { console.error(String(error)); process.exitCode = 1; }
}
