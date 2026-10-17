import { expect, setDefaultTimeout, test } from "bun:test";
import { execFile, spawn } from "node:child_process";
import { once } from "node:events";
import { mkdir, mkdtemp, readdir, rm, writeFile } from "node:fs/promises";
import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";
import neo4j from "neo4j-driver";

setDefaultTimeout(60_000);
const ROOT = fileURLToPath(new URL("../../", import.meta.url));

test("the projection method answers over the daemon socket and ops projection write prunes only stale projection files", async () => {
  const uri = process.env["ANAMNESIS_TEST_NEO4J_URI"], password = process.env["ANAMNESIS_TEST_NEO4J_PASSWORD"];
  if (!uri || !password) throw new Error("isolated graph credentials required");
  const { RpcClient } = await import(`${ROOT}dist/anamnesis-client.mjs`);
  const root = await mkdtemp("/tmp/anamnesis-projection-"), token = randomUUID();
  const driver = neo4j.driver(uri, neo4j.auth.basic("neo4j", password), { disableLosslessIntegers: true });
  await driver.executeQuery("MATCH (n) DETACH DELETE n");
  const env = { ...process.env, ANAMNESIS_RUNTIME_ROOT: root, ANAMNESIS_RUNTIME_TOKEN: token, ANAMNESIS_NEO4J_URI: uri, ANAMNESIS_NEO4J_PASSWORD: password };
  const child = spawn("node", ["dist/anamnesis-daemon.mjs"], { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"], env });
  const exited = once(child, "exit");
  child.stderr.on("data", chunk => process.stderr.write(chunk));
  const lines = createInterface({ input: child.stdout });
  const listening = new Promise<void>((resolve, reject) => {
    const timer = setTimeout(() => reject(new Error("daemon readiness deadline")), 20_000);
    lines.on("line", line => { if (JSON.parse(line).event === "listening") { clearTimeout(timer); resolve(); } });
    child.once("exit", code => { clearTimeout(timer); reject(new Error(`daemon exited ${code}`)); });
  });
  let client: { request(method: string, params: unknown): Promise<any>; close(): Promise<void> } | undefined;
  try {
    await listening;
    client = await RpcClient.connect(`${root}/anamnesis.sock`, token, "receipt");
    expect((await client!.request("status", {})).capabilities.projection).toBe(true);
    const snapshot = await client!.request("projection", { T: "2029-01-01T00:00:00Z" });
    expect(snapshot).toMatchObject({ T: "2029-01-01T00:00:00.000Z", entities: [] });
    expect(Date.parse(snapshot.generated_at)).not.toBeNaN();

    const dir = `${root}/projection`;
    await mkdir(dir);
    await writeFile(`${dir}/gone-0192f3a1b2c3.md`, "stale\n"); await writeFile(`${dir}/notes.md`, "mine\n");
    const ops = await promisify(execFile)("node", ["dist/anamnesis-ops.mjs", "projection", "write", dir], { cwd: ROOT, env });
    expect(JSON.parse(ops.stdout.trim().split("\n").at(-1)!)).toEqual({ entities: 0, facts: 0, written: 0, removed: 1, dir });
    expect(await readdir(dir)).toEqual(["notes.md"]);

    await client!.request("shutdown", {});
    expect((await exited)[0]).toBe(0);
  } finally {
    await client?.close();
    if (child.exitCode === null && child.signalCode === null) { child.kill("SIGKILL"); await exited; }
    await driver.close();
    await rm(root, { recursive: true, force: true });
  }
});
