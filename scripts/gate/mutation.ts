// Mutation gate. Stryker's command runner re-runs one fixed command per mutant, so a whole-suite command
// prices every mutant at the whole suite and times out under concurrency. Each source is instead mutated
// against only the pure tests that load it, and only on the lines those tests execute (per-test lcov), so a
// survivor is a mutant on an executed line that no running test noticed. Sources no pure test loads are pinned in
// mutation-untested.txt so that set can neither grow nor go stale unnoticed. Exit 1 on any surviving,
// errored or unpinned mutant, and on a group Stryker generates no mutants for; timeouts count as detections but
// are printed so load-induced ones are visible.
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join, relative } from "node:path";
import { sourceFiles } from "./sources.ts";

const PURE_LIST = "scripts/qa/pure-tests.txt";
const UNTESTED_LIST = "scripts/gate/mutation-untested.txt";
const WORKERS = 8;
/** A pure test that hangs under --coverage would otherwise stall the gate before any mutant runs. */
const COVERAGE_DEADLINE_MS = 600_000;

type Status = "Killed" | "Survived" | "NoCoverage" | "CompileError" | "RuntimeError" | "Timeout" | "Ignored" | "Pending";
interface Mutant { status: Status; mutatorName: string; replacement?: string; location: { start: { line: number; column: number } } }
interface Report { files: Record<string, { mutants: Mutant[] }> }
/** Lines lcov reports for a source (reported) and the subset one test executed (hit). */
interface Executed { hit: Set<number>; reported: Set<number> }
/** Unions over every test that loads the source. */
interface Coverage { tests: string[]; covered: Set<number>; reported: Set<number> }
interface Group { tests: string[]; sources: Map<string, Coverage> }
interface Tally { mutants: number; killed: number; timeouts: string[]; survived: string[]; errors: string[]; mutantFree: string[] }
const tally = (): Tally => ({ mutants: 0, killed: 0, timeouts: [], survived: [], errors: [], mutantFree: [] });

/** A killed gate must take its children with it: an orphaned Stryker master keeps eight workers busy for
 * hours. Each child is killed by its own handler; the non-zero exit then fails the run and cleanup proceeds.
 * A deadline SIGKILLs the child the same way (Stryker bounds its own test runs with timeoutMS). */
async function run(cmd: string[], env: Record<string, string>, deadlineMs?: number): Promise<{ code: number; out: string; timedOut: boolean }> {
  const child = Bun.spawn(cmd, { env: { ...process.env, ...env }, stdout: "pipe", stderr: "pipe" });
  const forward = (signal: NodeJS.Signals) => () => child.kill(signal);
  const onTerm = forward("SIGTERM"), onInt = forward("SIGINT");
  process.once("SIGTERM", onTerm).once("SIGINT", onInt);
  let timedOut = false;
  const deadline = deadlineMs === undefined ? undefined : setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, deadlineMs);
  try {
    const [out, err] = await Promise.all([new Response(child.stdout).text(), new Response(child.stderr).text()]);
    return { code: await child.exited, out: out + err, timedOut };
  } finally {
    clearTimeout(deadline);
    process.off("SIGTERM", onTerm).off("SIGINT", onInt);
  }
}

/** Lines each shipped source executes under one test: lcov DA records with a non-zero count. */
async function executedLines(test: string, dir: string, shipped: Set<string>): Promise<Map<string, Executed>> {
  const result = await run(["bun", "test", "--coverage", "--coverage-reporter=lcov", `--coverage-dir=${dir}`, test], {}, COVERAGE_DEADLINE_MS);
  if (result.code !== 0) throw new Error(`${test} ${result.timedOut ? "exceeded the coverage deadline" : "fails before any mutation"} (exit ${result.code}):\n${result.out}`);
  const files = new Map<string, Executed>();
  let current: Executed | undefined;
  for (const line of (await readFile(join(dir, "lcov.info"), "utf8")).split("\n")) {
    if (line.startsWith("SF:")) {
      const file = relative(process.cwd(), line.slice(3));
      current = shipped.has(file) ? { hit: new Set(), reported: new Set() } : undefined;
      if (current) files.set(file, current);
    } else if (current && line.startsWith("DA:")) {
      const number = Number(line.slice(3).split(",")[0]);
      current.reported.add(number);
      if (!line.endsWith(",0")) current.hit.add(number);
    }
  }
  return files;
}

async function mapCoverage(tests: string[], shipped: Set<string>, scratch: string): Promise<Map<string, Coverage>> {
  const bySource = new Map<string, Coverage>();
  const queue = tests.map((test, index) => async () => {
    for (const [source, { hit, reported }] of await executedLines(test, join(scratch, String(index)), shipped)) {
      if (hit.size === 0) continue;
      const entry = bySource.get(source) ?? { tests: [], covered: new Set<number>(), reported: new Set<number>() };
      entry.tests.push(test);
      for (const line of hit) entry.covered.add(line);
      for (const line of reported) entry.reported.add(line);
      bySource.set(source, entry);
    }
  });
  const workers = Array.from({ length: WORKERS }, async () => {
    try { for (let job = queue.shift(); job; job = queue.shift()) await job(); }
    catch (error) { queue.length = 0; throw error; }
  });
  // Every worker settles before a failure propagates, so no coverage child is still writing when scratch is removed.
  for (const result of await Promise.allSettled(workers)) if (result.status === "rejected") throw result.reason;
  return bySource;
}

function groupBySuite(bySource: Map<string, Coverage>): Group[] {
  const groups = new Map<string, Group>();
  for (const [source, coverage] of [...bySource].sort(([a], [b]) => a.localeCompare(b))) {
    const tests = [...coverage.tests].sort(), key = tests.join(" ");
    const group = groups.get(key) ?? { tests, sources: new Map() };
    group.sources.set(source, coverage);
    groups.set(key, group);
  }
  return [...groups.values()];
}

/** Stryker mutate entries for the executed lines, as inclusive line ranges. Stryker drops a mutant unless
 * it sits inside one range, and bun's lcov reports no entry for braces, blank lines or comments, so a range
 * runs on through unreported lines, stopping only at a line lcov reports unexecuted (or the file's ends):
 * the closing brace of a whole-block mutant is then always inside the range of the block it closes. */
function ranges(source: string, { covered, reported }: Coverage, lastLine: number): string[] {
  const lines = [...covered].sort((a, b) => a - b), out: string[] = [];
  const unexecuted = (line: number): boolean => reported.has(line) && !covered.has(line);
  const executedThrough = (from: number, to: number): boolean => {
    for (let line = from + 1; line < to; line += 1) if (unexecuted(line)) return false;
    return true;
  };
  for (let i = 0; i < lines.length;) {
    let end = i;
    while (end + 1 < lines.length && executedThrough(lines[end]!, lines[end + 1]!)) end += 1;
    let start = lines[i]!, stop = lines[end]!;
    while (start > 1 && !unexecuted(start - 1)) start -= 1;
    while (stop < lastLine && !unexecuted(stop + 1)) stop += 1;
    out.push(`${source}:${start}-${stop}`);
    i = end + 1;
  }
  return out;
}

async function mutate(group: Group, report: string): Promise<Tally> {
  const command = `bun scripts/gate/stryker-sandbox.ts && bun test ${group.tests.join(" ")}`;
  const lineCount = (text: string): number => text.split("\n").length - (text.endsWith("\n") ? 1 : 0);
  const mutateArg = (await Promise.all([...group.sources].map(async ([source, coverage]) => ranges(source, coverage, lineCount(await readFile(source, "utf8")))))).flat().join(",");
  const result = await run(["bunx", "stryker", "run", "--mutate", mutateArg, "--concurrency", String(WORKERS)], { STRYKER_TEST_COMMAND: command, STRYKER_REPORT: report });
  const parsed: Report = JSON.parse(await readFile(report, "utf8").catch(() => { throw new Error(`stryker produced no report (exit ${result.code}):\n${result.out.slice(-4000)}`); }));
  if (typeof parsed.files !== "object" || parsed.files === null) throw new Error(`stryker report ${report} has no files entry`);
  const counts = tally();
  // A source whose executed lines hold nothing mutable (re-export index files) is absent from the report; that is
  // listed, not failed. A group with no mutants at all means the mutate ranges were dropped and is a failure.
  counts.mutantFree = [...group.sources.keys()].filter(source => !(source in parsed.files));
  for (const file of Object.keys(parsed.files)) if (!group.sources.has(file)) throw new Error(`stryker report names ${file}, which is not in the group: key format drift`);
  for (const [file, { mutants }] of Object.entries(parsed.files)) for (const mutant of mutants) {
    counts.mutants += 1;
    const where = `${file}:${mutant.location.start.line}:${mutant.location.start.column} ${mutant.mutatorName} -> ${JSON.stringify(mutant.replacement ?? "")}`;
    if (mutant.status === "Killed") counts.killed += 1;
    else if (mutant.status === "Timeout") counts.timeouts.push(where);
    else if (mutant.status === "CompileError" || mutant.status === "RuntimeError" || mutant.status === "Ignored") counts.errors.push(`${mutant.status} ${where}`);
    else counts.survived.push(where);
  }
  if (counts.mutants === 0) throw new Error(`stryker generated no mutants for ${[...group.sources.keys()].join(" ")} (exit ${result.code}):\n${result.out.slice(-4000)}`);
  // The break threshold makes a survivor exit non-zero; a non-zero exit the report does not explain is a runner failure.
  if (result.code !== 0 && counts.survived.length === 0) throw new Error(`stryker exited ${result.code} with no survivor in ${report}:\n${result.out.slice(-4000)}`);
  return counts;
}

function list(title: string, lines: string[]): void {
  if (lines.length > 0) console.log(`${title} (${lines.length}):\n${lines.map(line => `  ${line}`).join("\n")}`);
}

const scratch = await mkdtemp(join(tmpdir(), "anamnesis-mutation-"));
try {
  const tests = (await readFile(PURE_LIST, "utf8")).split("\n").filter(Boolean);
  const shipped = sourceFiles();
  const bySource = await mapCoverage(tests, new Set(shipped), scratch);
  const groups = groupBySuite(bySource);
  const untested = shipped.filter(source => !bySource.has(source));
  const pinned = (await readFile(UNTESTED_LIST, "utf8")).split("\n").filter(Boolean);
  const unpinned = untested.filter(source => !pinned.includes(source)), stale = pinned.filter(source => !untested.includes(source));
  const coveredLines = [...bySource.values()].reduce((sum, { covered }) => sum + covered.size, 0);
  const totalLines = [...bySource.values()].reduce((sum, { reported }) => sum + reported.size, 0);
  console.log(`mutation: ${bySource.size} sources loaded by ${tests.length} pure tests in ${groups.length} groups; mutating the ${coveredLines}/${totalLines} lines they execute; ${untested.length} sources no pure test loads`);
  const total = tally();
  for (const [index, group] of groups.entries()) {
    const started = Date.now();
    const counts = await mutate(group, join(scratch, `report-${index}.json`));
    total.mutants += counts.mutants; total.killed += counts.killed;
    for (const key of ["timeouts", "survived", "errors", "mutantFree"] as const) total[key].push(...counts[key]);
    console.log(`group ${index + 1}/${groups.length}: ${group.sources.size} sources, ${group.tests.length} tests, ${counts.mutants} mutants, ${counts.killed} killed, ${counts.timeouts.length} timeouts, ${counts.survived.length} survived, ${counts.errors.length} errors, ${Math.round((Date.now() - started) / 1000)}s [${[...group.sources.keys()].join(" ")}]`);
  }
  list(`untested by pure suites, pinned in ${UNTESTED_LIST}`, untested);
  list("untested but not pinned", unpinned);
  list("pinned but now tested", stale);
  list("no mutants on executed lines (re-export only?)", total.mutantFree);
  list("timeouts (detected; verify loops rather than load)", total.timeouts);
  list("errors", total.errors);
  list("survived", total.survived);
  const failed = total.survived.length + total.errors.length + unpinned.length + stale.length > 0;
  const verdict = failed ? "FAILED" : total.timeouts.length > 0 ? "ok WITH TIMEOUTS (counted as detections; verify the list above)" : "ok";
  console.log(`mutation ${verdict}: ${total.mutants} mutants on pure-executed lines, ${total.killed} killed, ${total.timeouts.length} timeouts, ${total.survived.length} survived, ${total.errors.length} errors; ${untested.length} sources untested by pure suites`);
  process.exitCode = failed ? 1 : 0;
} finally { await rm(scratch, { recursive: true, force: true }); }
