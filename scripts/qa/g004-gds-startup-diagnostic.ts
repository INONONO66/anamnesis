import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { chmod, copyFile, mkdir, readFile, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { BINARY_PINS, CONFIG, EVIDENCE, ROOT, cleanupGdsContainer, admitArtifact } from "./g004-gds-solver-20.runner.ts";

const owner = `diagnostic-${randomUUID()}`;
const name = `anamnesis-gds-${owner}`;
const directory = join(EVIDENCE, owner);
await mkdir(directory, { mode: 0o700 });
const secret = randomBytes(32).toString("hex");
const redact = (text: string) => text.replaceAll(secret, "<redacted>");
let sequence = 0;
async function command(argv: string[]) {
  const prefix = join(directory, String(++sequence).padStart(3, "0"));
  await writeFile(`${prefix}.command.json`, JSON.stringify({ argv, cwd: ROOT, timeoutMs: 120000 }));
  const child = spawn(argv[0] ?? "", argv.slice(1), { cwd: ROOT, stdio: ["ignore", "pipe", "pipe"] });
  const result = await new Promise<{ code: number | null; signal: NodeJS.Signals | null; stdout: string; stderr: string }>((resolve, reject) => {
    let stdout = "", stderr = "";
    const timer = setTimeout(() => child.kill("SIGKILL"), 120000);
    child.stdout.on("data", bytes => { stdout += bytes; });
    child.stderr.on("data", bytes => { stderr += bytes; });
    child.once("error", error => { clearTimeout(timer); reject(error); });
    child.once("close", (code, signal) => { clearTimeout(timer); resolve({ code, signal, stdout, stderr }); });
  });
  await writeFile(`${prefix}.result.json`, redact(JSON.stringify(result, null, 2)));
  return result;
}
let attempted = false;
const envFile = join(directory, "secret.env");
try {
  const admission = await admitArtifact(join(EVIDENCE, "acquisition/admission.json"));
  const plugins = join(directory, "plugins");
  await mkdir(plugins, { mode: 0o755 });
  const plugin = join(plugins, "neo4j-graph-data-science-2.13.12.jar");
  await copyFile(admission.jar.path, plugin);
  await chmod(plugin, 0o444);
  await writeFile(envFile, `NEO4J_AUTH=neo4j/${secret}\n${CONFIG.join("\n")}\n`, { mode: 0o600 });
  attempted = true;
  const created = await command(["docker", "create", "--name", name, "--pull", "never", "--platform", BINARY_PINS.platform,
    "--network", "none", "--label", `omo.owner=${owner}`, "--label", "omo.task=gds-solver-20",
    "--env-file", envFile,
    "--mount", `type=bind,src=${plugins},dst=/plugins,readonly`,
    "--mount", `type=bind,src=${join(EVIDENCE, "BootstrapDiagnostic.java")},dst=/BootstrapDiagnostic.java,readonly`,
    BINARY_PINS.image,
    "/opt/java/openjdk/bin/java", "-Xmx512m", "-cp", "/plugins/*:/var/lib/neo4j/conf/*:/var/lib/neo4j/lib/*",
    "/BootstrapDiagnostic.java", "--home-dir=/var/lib/neo4j", "--config-dir=/var/lib/neo4j/conf", "--console-mode"]);
  assert.equal(created.code, 0);
  const id = created.stdout.trim();
  assert.match(id, /^[a-f0-9]{64}$/);
  const result = await command(["docker", "start", "--attach", id]);
  console.log(redact(result.stdout));
  console.log(redact(result.stderr));
  await writeFile(join(directory, "diagnosis.json"), JSON.stringify({
    admissionJarHash: admission.jar.sha256, bootstrapSource: await readFile(join(EVIDENCE, "BootstrapDiagnostic.java"), "utf8"),
    status: result.code, signal: result.signal, graphQualification: false,
  }, null, 2));
  assert.ok(result.stderr.includes("BOOTSTRAP_STATUS="), "bootstrap diagnostic did not execute");
} finally {
  try {
    if (attempted) {
      const cleanup = await cleanupGdsContainer(command, name, owner, join(directory, "logs"));
      await writeFile(join(directory, "cleanup.json"), JSON.stringify(cleanup, null, 2));
      assert.ok(cleanup.containerAbsent && cleanup.volumesAbsent, "diagnostic cleanup incomplete");
    }
  } finally { await rm(envFile, { force: true }); }
}
console.log(`DIAGNOSTIC_EVIDENCE=${directory}`);
