import { expect, test } from "bun:test";
import { spawn, type ChildProcess } from "node:child_process";
import { once } from "node:events";
import { existsSync, watch } from "node:fs";
import { createInterface } from "node:readline";
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { createHash, randomUUID } from "node:crypto";
import { fileURLToPath } from "node:url";
import neo4j from "neo4j-driver";
import { Manifest } from "../../packages/archive/src/manifest.ts";
import { revisionOf, type ArchiveRecord } from "../../packages/archive/src/record.ts";

const ROOT = fileURLToPath(new URL("../../", import.meta.url));
const OUT = process.env["ANAMNESIS_G9_SURFACE_OUT"]
  ?? "/Users/ino/Develop/anamnesis/.omo/evidence/ulw/01a11b8c-9f0b-71f3-ac0c-cb1048614a4e/G004-g9-archive-admit/surface";
const SESSIONS = [["g9-s-a", 60], ["g9-s-b", 60]] as const;
const N = SESSIONS.reduce((sum, [, count]) => sum + count, 0);
const hash = (value: string): string => createHash("sha256").update(value).digest("hex");

/** One omo archive: two sessions, alternating user/assistant messages a second apart. */
async function archive(): Promise<string> {
  const root = await mkdtemp("/tmp/g9-archive-admit-archive-");
  const manifest = Manifest.open(root);
  for (const [session, count] of SESSIONS) {
    const records = Array.from({ length: count }, (_, n): ArchiveRecord => {
      const occurred_at = new Date(Date.UTC(2026, 9, 10, 12, 0, n)).toISOString(), text = `${session} archived turn ${n}`, actor = n % 2 === 0 ? "user" : "assistant";
      return { origin: { source: "omo", session, actor, record: `${session}-r${n}` }, seq: n, role: actor, occurred_at, time_precision: "second", text,
        kind: "message", canonical_kind: "message", revision: revisionOf(occurred_at, text), file: { sha256: hash(session), path: `/fixture/${session}.jsonl`, line: n + 1 } };
    });
    manifest.insertSession({ sha256: hash(session), source: "omo", sessionId: session, bytes: 1, path: `/fixture/${session}.jsonl`, collectedAt: "2026-10-10T12:00:00.000Z" }, records);
  }
  manifest.close();
  return root;
}

async function daemon(uri: string, password: string) {
  const root = await mkdtemp("/tmp/g9-archive-admit-runtime-"), token = randomUUID();
  const child = spawn("node", ["dist/anamnesis-daemon.mjs"], { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"], env: { ...process.env,
    ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password } });
  const exited = once(child, "exit");
  child.stderr.on("data", chunk => process.stderr.write(chunk));
  const lines = createInterface({ input: child.stdout });
  const listening = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(Error("listening deadline")), 20000);
    lines.on("line", line => { if (JSON.parse(line).event === "listening") { clearTimeout(timer); resolve(); } });
  });
  await Promise.race([listening, exited.then(([code]) => { throw Error(`daemon exit ${code}`); })]);
  return { socket: `${root}/anamnesis.sock`, token, async stop() {
    if (child.exitCode === null && child.signalCode === null) { child.kill("SIGTERM"); await exited; }
    lines.close(); await rm(root, { recursive: true, force: true });
  } };
}

function admit(archiveDir: string, target: { socket: string; token: string }): { child: ChildProcess; done: Promise<{ code: number | null; signal: string | null; stdout: string; stderr: string }> } {
  const child = spawn(process.execPath, ["packages/archive/src/cli.ts", "admit", "--archive", archiveDir, "--socket", target.socket, "--token", target.token],
    { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"] });
  let stdout = "", stderr = "";
  child.stdout!.on("data", chunk => { stdout += chunk; });
  child.stderr!.on("data", chunk => { stderr += chunk; });
  return { child, done: once(child, "exit").then(([code, signal]) => ({ code: code as number | null, signal: signal as string | null, stdout, stderr })) };
}

const report = (stdout: string) => JSON.parse(stdout.trim().split("\n").at(-1)!);

test("archive admit lands every record once on the real daemon, dedupes a rerun and resumes after SIGTERM", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  await mkdir(OUT, { recursive: true });
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  const query = async (cypher: string) => (await driver.executeQuery(cypher)).records.map(row => row.toObject());
  const episodes = async () => (await query("MATCH (e:Element:Episode) RETURN count(e) AS n"))[0]!["n"] as number;
  const duplicateRevisions = () => query("MATCH (e:Element:Episode) WITH e.revision_key AS key, count(*) AS n WHERE n > 1 RETURN key, n");
  const write = (name: string, body: unknown) => writeFile(`${OUT}/${name}.json`, JSON.stringify(body, null, 2) + "\n");
  const archives: string[] = [];
  let running: Awaited<ReturnType<typeof daemon>> | undefined;
  try {
    await query("MATCH (n) DETACH DELETE n");
    running = await daemon(uri, password);
    const first = await archive(); archives.push(first);

    const one = await admit(first, running).done;
    const oneCount = await episodes();
    await write("01-admit-first", { records: N, exit: one.code, report: report(one.stdout), stderr: one.stderr, episode_count: oneCount,
      cursor: JSON.parse(await readFile(`${first}/admit/omo.cursor.json`, "utf8")) });
    expect(one.code).toBe(0);
    expect(report(one.stdout)).toEqual({ source: "omo", records: N, created: N, duplicates: 0, failed: 0, skipped: 0, since: null, resumed_from: null });
    expect(oneCount).toBe(N);

    const two = await admit(first, running).done;
    const twoCount = await episodes();
    await write("02-admit-again", { records: N, exit: two.code, report: report(two.stdout), stderr: two.stderr, episode_count: twoCount });
    expect(two.code).toBe(0);
    expect(report(two.stdout)).toEqual({ source: "omo", records: N, created: 0, duplicates: N, failed: 0, skipped: 0, since: null, resumed_from: null });
    expect(twoCount).toBe(N);

    await running.stop(); running = undefined;
    await query("MATCH (n) DETACH DELETE n");
    running = await daemon(uri, password);
    const second = await archive(); archives.push(second);
    const cursorPath = `${second}/admit/omo.cursor.json`;
    await mkdir(`${second}/admit`);
    const watcher = watch(`${second}/admit`);
    const appeared = new Promise<void>(resolve => watcher.on("change", () => { if (existsSync(cursorPath)) resolve(); }));
    const killed = admit(second, running);
    const raced = await Promise.race([appeared.then(() => "cursor"), killed.done.then(() => "exit")]);
    watcher.close();
    expect(raced).toBe("cursor");
    killed.child.kill("SIGTERM");
    const three = await killed.done;
    const cursor = JSON.parse(await readFile(cursorPath, "utf8"));
    const threeCount = await episodes();
    await write("03-kill", { records: N, exit: three.code, signal: three.signal, report: report(three.stdout), stderr: three.stderr, cursor, episode_count: threeCount });
    expect(three.code).toBe(143);
    expect(cursor.status).toBe("in_progress");
    expect(cursor.created).toBe(threeCount);
    expect(threeCount).toBeLessThan(N);

    const four = await admit(second, running).done;
    const fourReport = report(four.stdout), fourCount = await episodes(), duplicates = await duplicateRevisions();
    const finalCursor = JSON.parse(await readFile(cursorPath, "utf8"));
    await write("04-resume", { records: N, exit: four.code, report: fourReport, stderr: four.stderr, cursor: finalCursor, episode_count: fourCount, duplicate_revision_rows: duplicates });
    expect(four.code).toBe(0);
    expect(fourReport.resumed_from).toEqual(cursor.last);
    expect(fourReport).toMatchObject({ created: N - threeCount, duplicates: 0, failed: 0 });
    expect(fourCount).toBe(N);
    expect(duplicates).toEqual([]);
    expect(finalCursor).toMatchObject({ status: "complete", created: N, last: { session_id: "g9-s-b", index: 59 } });
  } finally {
    await running?.stop();
    await driver.close();
    await Promise.all(archives.map(dir => rm(dir, { recursive: true, force: true })));
  }
}, 180000);
