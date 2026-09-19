import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { join } from "node:path";
import type { startProcess } from "./runtime-scenarios.ts";

export const SOAK_TARGET_MS = 24 * 60 * 60 * 1000;
export const RELEASE_CASES = ["uds-ingest", "object-spool-crashes", "outage-drain-50", "source-resume", "managed-ingest-restart", "v01-acceptance"] as const;
export const SOAK_CASES = ["uds-ingest", "managed-ingest-restart"] as const;
export const G006_CASES = new Set(["release-acceptance", "soak-24h"]);
export interface SoakContract { durationMs: number; maxIterations: number }

/** The continuous runner deliberately has no iteration admission budget. The
 * caller owns one daemon for the whole interval and supplies event-backed
 * synchronization for every action. `now` must be monotonic. */
export interface ContinuousSoakDriver {
  launchOwnedDaemon: () => Promise<void>;
  subscribe: () => Promise<void>;
  traffic: (sequence: number) => Promise<void>;
  recover: (sequence: number) => Promise<void>;
  terminal: () => Promise<void>;
  cleanup: () => Promise<void>;
  hashEvidence: () => Promise<Record<string, string>>;
}
export interface ContinuousSoakResult {
  status: "UNKNOWN" | "COMPLETE" | "FAIL";
  qualification: "UNKNOWN";
  reason: string;
  startedAt: number;
  completedAt?: number;
  trafficCount: number;
  recoveryCount: number;
  hashes?: Record<string, string>;
  error?: string;
  cleanupError?: string;
}

/** State machine for G006's genuine one-process soak. No timer, sleep, or
 * polling is hidden here: the driver returns only after an observed event or
 * natural terminal completion. Before that terminal event the result remains
 * UNKNOWN, including on interruption or driver failure. */
export async function runContinuousSoak(options: {
  durationMs: number;
  now: () => number;
  signal: AbortSignal;
  driver: ContinuousSoakDriver;
  record: (result: ContinuousSoakResult) => Promise<void>;
}): Promise<ContinuousSoakResult> {
  const startedAt = options.now();
  const result: ContinuousSoakResult = { status: "UNKNOWN", qualification: "UNKNOWN", reason: "incomplete", startedAt, trafficCount: 0, recoveryCount: 0 };
  const save = async () => options.record({ ...result });
  try {
    await options.driver.launchOwnedDaemon();
    await options.driver.subscribe();
    await save();
    while (options.now() - startedAt < options.durationMs) {
      options.signal.throwIfAborted();
      const sequence = result.trafficCount + result.recoveryCount;
      await options.driver.traffic(sequence);
      result.trafficCount++;
      if (result.trafficCount % 2 === 0) { await options.driver.recover(sequence); result.recoveryCount++; }
      await save();
    }
    // Completion is admitted only by the natural monotonic terminal boundary.
    options.signal.throwIfAborted();
    await options.driver.terminal();
    result.hashes = await options.driver.hashEvidence();
    options.signal.throwIfAborted();
    result.status = "COMPLETE"; result.reason = "natural_duration_completed"; result.completedAt = options.now();
    await save();
  } catch (error) {
    result.status = "UNKNOWN";
    delete result.completedAt;
    result.error = error instanceof Error ? error.stack ?? String(error) : String(error);
    result.reason = options.signal.aborted ? "interrupted" : `incomplete: ${String(error)}`;
    await save();
  } finally {
    try { await options.driver.cleanup(); }
    catch (error) {
      result.cleanupError = error instanceof Error ? error.stack ?? String(error) : String(error);
      if (result.status === "COMPLETE") { result.status = "FAIL"; result.reason = "cleanup_failed"; }
    }
    await save();
  }
  return result;
}

export interface AggregateResult {
  status: "COMPLETE" | "UNKNOWN" | "FAIL";
  qualification: "UNKNOWN";
  reason: string;
  elapsedMs: number;
  completedIterations: number;
  runs: { iteration: number; caseName: string; evidence?: string; error?: string }[];
}

/** Duration bounds admission, not cleanup. An admitted case keeps its existing
 * bounded process/DB deadlines. No synthetic clock, idle wait or parallel load. */
export async function runBoundedScenarios(options: {
  caseName: string;
  soak?: SoakContract;
  now: () => number;
  signal: AbortSignal;
  run: (caseName: string, iteration: number) => Promise<string>;
  record: (result: AggregateResult) => Promise<void>;
}): Promise<AggregateResult> {
  const start = options.now();
  const result: AggregateResult = { status: "UNKNOWN", qualification: "UNKNOWN", reason: "incomplete", elapsedMs: 0, completedIterations: 0, runs: [] };
  const snapshot = async () => { result.elapsedMs = options.now() - start; await options.record(result); };
  const cases = options.soak ? SOAK_CASES : RELEASE_CASES;
  await snapshot();
  for (let iteration = 1; iteration <= (options.soak?.maxIterations ?? 1); iteration++) {
    for (const caseName of cases) {
      if (options.signal.aborted) { result.reason = "interrupted"; await snapshot(); return result; }
      if (options.soak && options.now() - start >= options.soak.durationMs) {
        result.status = options.now() - start >= SOAK_TARGET_MS && result.completedIterations > 0 ? "COMPLETE" : "UNKNOWN";
        result.reason = "duration_budget_exhausted";
        await snapshot(); return result;
      }
      const run: AggregateResult["runs"][number] = { iteration, caseName };
      result.runs.push(run);
      await snapshot(); // Persist admission before acquisition/child execution.
      try { run.evidence = await options.run(caseName, iteration); }
      catch (error) {
        run.error = String(error);
        result.status = options.signal.aborted ? "UNKNOWN" : "FAIL";
        result.reason = options.signal.aborted ? "interrupted" : "surface_failed";
        await snapshot(); return result;
      }
      await snapshot();
    }
    result.completedIterations++;
    await snapshot();
  }
  result.elapsedMs = options.now() - start;
  result.status = options.signal.aborted ? "UNKNOWN" : !options.soak || result.elapsedMs >= SOAK_TARGET_MS ? "COMPLETE" : "UNKNOWN";
  result.reason = options.signal.aborted ? "interrupted" : options.soak ? "iteration_budget_exhausted" : "registered_surfaces_complete";
  await snapshot();
  return result;
}

export class ScenarioIncompleteError extends Error {}

/** Compose only existing real surfaces. Each nested runner owns its own DB and
 * cleanup; recovery's relative imports resolve against our private fresh build. */
export async function executeG006(options: { caseName: string; soak?: SoakContract }, evidence: string, dependencies: {
  startProcess: typeof startProcess;
  runCase: (caseName: string, evidenceRoot: string, workspace: string, signal: AbortSignal) => Promise<string>;
  signal?: AbortSignal;
  createContinuousDriver?: (evidence: string, workspace: string, signal: AbortSignal) => Promise<ContinuousSoakDriver> | ContinuousSoakDriver;
}) {
  const controller = new AbortController();
  const interrupt = () => controller.abort();
  process.on("SIGINT", interrupt); process.on("SIGTERM", interrupt);
  const record = (file: string, value: unknown) => writeFile(join(evidence, file), JSON.stringify(value, null, 2) + "\n");
  const workspace = join(evidence, "workspace");
  let outcome: AggregateResult | ContinuousSoakResult | undefined;
  let exitCode = 0;
  const signal = dependencies.signal ? AbortSignal.any([dependencies.signal, controller.signal]) : controller.signal;
  try {
    const effectiveSoak = options.soak;
    await record("contract.json", { caseName: options.caseName, soak: effectiveSoak ?? null, durationMs: effectiveSoak?.durationMs ?? null, targetDurationMs: SOAK_TARGET_MS,
      durationSemantics: "monotonic admission budget; in-flight case and owned cleanup finish under existing deadlines",
      cases: options.soak ? ["continuous-traffic-recovery"] : RELEASE_CASES, concurrency: 1, resourceLifetime: options.soak ? "one owned Node daemon/root and Neo4j endpoint for the full interval" : "fresh owned DB/root per case",
      qualification: "UNKNOWN", limitations: options.soak ? ["not terminal until natural duration", "G004/G005 completeness not covered", "no CI/review/merge qualification"] : ["not continuous-process longevity", "G004/G005 completeness not covered", "no CI/review/merge qualification"] });
    await record("result.json", { status: "UNKNOWN", qualification: "UNKNOWN", reason: "building" });
    await mkdir(join(workspace, "app/anamnesis"), { recursive: true });
    await mkdir(join(workspace, "dist"), { recursive: true });
    const hashes: Record<string, string> = {};
    const hash = (bytes: Uint8Array) => createHash("sha256").update(bytes).digest("hex");
    const source = "app/anamnesis/recovery.surface.mjs";
    const fixture = await readFile(new URL(`../../${source}`, import.meta.url));
    await writeFile(join(workspace, source), fixture);
    hashes[source] = hash(fixture);
    for (const [source, output] of [["main.ts", "anamnesis-daemon.mjs"], ["client.ts", "anamnesis-client.mjs"], ["ops.ts", "anamnesis-ops.mjs"]]) {
      signal.throwIfAborted();
      const command = ["build", `app/anamnesis/${source}`, "--target=node", "--outfile", join(workspace, "dist", output!)];
      const built = await dependencies.startProcess(process.execPath, command, { deadlineMs: 120_000, signal }).done;
      await record(`${output}.build.json`, { command: [process.execPath, ...command], ...built });
      if (built.code !== 0 || built.timedOut) throw new Error(`Current build failed: ${output}: ${built.output}`);
      hashes[`dist/${output}`] = hash(await readFile(join(workspace, "dist", output!)));
    }
    await record("artifacts.json", hashes);
    if (effectiveSoak) {
      if (!dependencies.createContinuousDriver) throw new Error("continuous soak driver is required");
      outcome = await runContinuousSoak({ durationMs: effectiveSoak.durationMs, now: () => performance.now(), signal,
        driver: await dependencies.createContinuousDriver(evidence, workspace, signal),
        record: result => record("result.json", result) });
      if (outcome.status === "COMPLETE" && effectiveSoak.durationMs < SOAK_TARGET_MS) {
        outcome.status = "UNKNOWN"; outcome.reason = "short_duration_completed";
        await record("result.json", outcome);
      }
    } else outcome = await runBoundedScenarios({ ...options, now: () => performance.now(), signal: controller.signal,
      run: async (caseName, iteration) => {
        const evidenceRoot = join(evidence, `iteration-${iteration}`, caseName);
        try {
          return await dependencies.runCase(caseName, evidenceRoot, workspace, controller.signal);
        } catch (error) { throw new Error(`${caseName} evidence root ${evidenceRoot}: ${String(error)}`); }
      }, record: result => record("result.json", result) });
    for (const [path, expected] of Object.entries(hashes)) {
      if (hash(await readFile(join(workspace, path))) !== expected) throw new Error(`Artifact changed during run: ${path}`);
    }
    if (outcome && outcome.status === "UNKNOWN") throw new ScenarioIncompleteError(`UNKNOWN: ${outcome.reason}; evidence: ${evidence}`);
    if (outcome && outcome.status === "FAIL") throw new Error(`Surface failed; evidence: ${evidence}`);
  } catch (error) {
    exitCode = error instanceof ScenarioIncompleteError || signal.aborted ? 2 : 1;
    if (!(error instanceof ScenarioIncompleteError) && (!outcome || outcome.status === "COMPLETE")) {
      await record("result.json", { ...outcome, status: signal.aborted ? "UNKNOWN" : "FAIL", qualification: "UNKNOWN", error: String(error) });
    }
    if (signal.aborted && !(error instanceof ScenarioIncompleteError)) throw new ScenarioIncompleteError(`Interrupted; evidence: ${evidence}; ${String(error)}`);
    throw error;
  } finally {
    process.off("SIGINT", interrupt); process.off("SIGTERM", interrupt);
    await record("cleanup.json", { signalHandlersRemoved: true, workspaceRetainedAsEvidence: workspace,
      resources: options.soak ? "continuous-cleanup.json records daemon/root; runner-result.json records owned container cleanup" : "nested case receipts record owned resources" });
    await record("exit.code", exitCode);
  }
  return evidence;
}
