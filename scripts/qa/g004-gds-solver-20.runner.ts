import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { randomBytes, randomUUID } from "node:crypto";
import { appendFile, chmod, copyFile, mkdir, mkdtemp, readFile, realpath, rm, stat, writeFile } from "node:fs/promises";
import { isAbsolute, join } from "node:path";
import { fileURLToPath } from "node:url";
import { exportFixedCsr, solveFixedCsr } from "../../packages/core/src/dynamics/ppr.ts";
import { compareScores, exactReference } from "./g004-ppr-oracle.ts";
import { analyticValues, augmentedEdges, checkProjection, generateFixtures, LIMITS, residual, sha256, validateInput, type Fixture } from "./g004-gds-solver-20.fixtures.ts";

export const ROOT = fileURLToPath(new URL("../../", import.meta.url)).replace(/\/$/, "");
export const EVIDENCE = join(ROOT, ".omo/evidence/g004-gds-solver-20");
export const BINARY_PINS = {
  image: "neo4j@sha256:037cf5756f0135cbfd66b739b6df7c7c4bb100f9ce11602f6f9538e17e02c74d",
  platform: "linux/arm64/v8", platformManifest: "sha256:77ad51ca3579a345bd5ea842234480b87319da4c9281b410d3d305b856b4e905",
  imageConfig: "sha256:c397ba43d7fe72a62933720d0526d1651422cdeec56295a02c302f7211d10f72", neo4jVersion: "5.26.30", gdsVersion: "2.13.12",
  runtime: "1.4.1", runtimePath: "/Users/ino/.local/share/mise/installs/bun/1.4.1/bin/bun",
};
export const PROJECTION = { orientation: "NATURAL", aggregation: "NONE", relationship: "TRANSITION", property: "weight", readConcurrency: 1, validateRelationships: true };
export const ALGORITHM = { sourceNodes: ["__sigma__:actual-Node"], dampingFactor: 0.85, relationshipWeightProperty: "weight", concurrency: 1, tolerance: 1e-12, maxIterations: 10000, scaler: "None" };
export function validateGdsEdition(edition: string, isLicensed: boolean): "default-community" {
  assert.ok(/^(community|unlicensed)$/i.test(edition) && isLicensed === false,
    "version_mismatch: default Community runtime required");
  return "default-community";
}
export const CONFIG = [
  "NEO4J_server_default__listen__address=127.0.0.1", "NEO4J_server_bolt_listen__address=127.0.0.1:7687",
  "NEO4J_server_http_enabled=false", "NEO4J_server_https_enabled=false", "NEO4J_dbms_security_procedures_unrestricted=gds.*",
  "NEO4J_dbms_security_procedures_allowlist=gds.*", "NEO4J_server_memory_heap_initial__size=512M",
  "NEO4J_server_memory_heap_max__size=512M", "NEO4J_server_memory_pagecache_size=128M",
];
export type Admission = {
  schemaVersion: 1; pins: typeof BINARY_PINS; projection: typeof PROJECTION; algorithm: typeof ALGORITHM;
  jar: { path: string; sha256: string; bytes: number }; archive: { path: string; sha256: string; bytes: number; url: string; metadataPath: string; acquiredAt: string };
  licenseEdition: "community"; offlineDisposableOnly: true; licenseReviewed: true;
  licenseFiles: { path: string; sha256: string }[];
  trust: { publisherSignatureVerified: false; description: string; jarUnsigned: true; licenseDiscrepancy: string };
};

export async function admitArtifact(path: string | undefined): Promise<Admission> {
  assert.ok(path && isAbsolute(path), "artifact_unavailable: explicit absolute GDS_BENCH_ARTIFACT_MANIFEST required");
  let manifest: Admission;
  try { manifest = JSON.parse(await readFile(path, "utf8")) as Admission; }
  catch (error) { throw new Error("artifact_unavailable: unreadable manifest", { cause: error }); }
  assert.equal(manifest.schemaVersion, 1, "artifact_unavailable: manifest schema");
  assert.deepEqual(manifest.pins, BINARY_PINS, "version_mismatch: admitted binary pins");
  assert.deepEqual(manifest.projection, PROJECTION, "projection_mismatch: admitted configuration");
  assert.deepEqual(manifest.algorithm, ALGORITHM, "version_mismatch: algorithm configuration");
  assert.equal(manifest.licenseEdition, "community");
  assert.equal(manifest.offlineDisposableOnly, true); assert.equal(manifest.licenseReviewed, true);
  assert.equal(manifest.trust.publisherSignatureVerified, false); assert.equal(manifest.trust.jarUnsigned, true);
  assert.ok(manifest.trust.description.length > 0 && manifest.trust.licenseDiscrepancy.length > 0);
  assert.equal(manifest.archive.url, "https://graphdatascience.ninja/neo4j-graph-data-science-2.13.12.zip");
  assert.ok(manifest.licenseFiles.length >= 3);
  for (const item of [manifest.jar, manifest.archive, ...manifest.licenseFiles]) {
    assert.ok(isAbsolute(item.path) && /^[a-f0-9]{64}$/.test(item.sha256), "artifact_unavailable: invalid content pin");
    const actual = await realpath(item.path);
    assert.ok(actual.startsWith(join(EVIDENCE, "acquisition") + "/"), "artifact_unavailable: evidence-private cache required");
    const bytes = await readFile(actual);
    assert.equal(sha256(bytes), item.sha256, "artifact_unavailable: content hash mismatch");
    if ("bytes" in item) assert.equal(bytes.length, item.bytes, "artifact_unavailable: length mismatch");
  }
  return manifest;
}

/** RFC4180-style scalar CSV, including doubled quotes and quoted newlines.
 * cypher-shell inserts a space after delimiters; only unquoted padding is trimmed. */
export function parseCsv(text: string, header: string[]): string[][] {
  const rows: string[][] = []; let row: string[] = [], field = "", quoted = false, closed = false, wasQuoted = false;
  const finish = () => { row.push(wasQuoted ? field : field.trim()); field = ""; closed = false; wasQuoted = false; };
  for (let i = 0; i < text.length; i++) {
    const c = text[i]!;
    if (quoted) {
      if (c === '"') { if (text[i + 1] === '"') { field += '"'; i++; } else { quoted = false; closed = true; } }
      else field += c;
    } else if (c === '"') { assert.ok(!closed && field.trim() === "", "transport_invalid: quote"); field = ""; quoted = true; wasQuoted = true; }
    else if (c === ',') { finish(); }
    else if (c === '\n' || c === '\r') {
      if (c === '\r' && text[i + 1] === '\n') i++;
      finish(); if (!(row.length === 1 && row[0] === "")) rows.push(row); row = [];
    } else { assert.ok(!closed || c === ' ' || c === '\t', "transport_invalid: trailing quote data"); if (!closed) field += c; }
  }
  assert.ok(!quoted, "transport_invalid: unterminated CSV quote");
  if (field.length || row.length || closed) { finish(); rows.push(row); }
  assert.deepEqual(rows.shift(), header, "transport_invalid: query-specific header");
  assert.ok(rows.every(r => r.length === header.length), "transport_invalid: row shape");
  return rows;
}
export function numeric(value: string): number {
  assert.ok(/^[+-]?(?:\d+(?:\.\d*)?|\.\d+)(?:[eE][+-]?\d+)?$/.test(value), "transport_invalid: number");
  const n = Number(value); assert.ok(Number.isFinite(n), "transport_invalid: nonfinite"); return n;
}
export function normalizedScores(f: Fixture, rows: string[][]) {
  assert.equal(rows.length, f.nodes.length + 1, "reference_residual_failed: score count");
  const scores = new Map<string, number>();
  for (const row of rows) { assert.equal(row.length, 2); const [id, raw] = row as [string, string]; const value = numeric(raw);
    assert.ok(value >= 0 && !scores.has(id) && (id === "__sigma__" || f.nodes.includes(id)), "reference_residual_failed: score IDs / sign"); scores.set(id, value); }
  assert.ok(scores.has("__sigma__"));
  const rawV = f.nodes.map(id => scores.get(id)!); const rawVTotal = rawV.reduce((a, b) => a + b, 0);
  assert.ok(Number.isFinite(rawVTotal) && rawVTotal > 0, "reference_residual_failed: nonpositive V mass");
  return { rawRows: rows, rawTotal: [...scores.values()].reduce((a, b) => a + b, 0), rawVTotal, rawV, values: rawV.map(v => v / rawVTotal) };
}

type CommandResult = { code: number | null; signal: NodeJS.Signals | null; stdout: string; stderr: string };
/** Recover ownership by the name reserved BEFORE create, not its stdout.
 * Failed log/stop commands remain failures but cannot bypass owned removal. */
export async function cleanupGdsContainer(command: (argv: string[]) => Promise<CommandResult>, name: string, owner: string, logDirectory?: string) {
  const inspected = await command(["docker", "inspect", name]);
  if (inspected.code !== 0) {
    assert.ok(/no such (object|container)/i.test(inspected.stderr), "cleanup_failed: ownership inspect");
    return { containerAbsent: true, volumesAbsent: true, cid: null, ownedVolumes: [], errors: [] };
  }
  const decoded: unknown = JSON.parse(inspected.stdout);
  assert.ok(Array.isArray(decoded) && decoded.length === 1, "cleanup_failed: ownership inspect shape");
  const container: unknown = decoded[0];
  assert.ok(container && typeof container === "object" && "Id" in container && "Config" in container
    && "Mounts" in container && "State" in container, "cleanup_failed: ownership fields");
  const { Id: id, Config: config, Mounts: mounts, State: state } = container;
  assert.ok(typeof id === "string" && /^[a-f0-9]{64}$/.test(id), "cleanup_failed: ownership ID");
  assert.ok(config && typeof config === "object" && "Labels" in config, "cleanup_failed: ownership labels");
  const labels = config.Labels;
  assert.ok(labels && typeof labels === "object" && "omo.owner" in labels && "omo.task" in labels
    && labels["omo.owner"] === owner && labels["omo.task"] === "gds-solver-20", "cleanup_failed: ownership mismatch");
  assert.ok(Array.isArray(mounts), "cleanup_failed: ownership mounts");
  assert.ok(state && typeof state === "object" && "Running" in state && typeof state.Running === "boolean",
    "cleanup_failed: ownership state");
  const ownedVolumes: string[] = [];
  for (const value of mounts) {
    const mount: unknown = value;
    assert.ok(mount && typeof mount === "object" && "Type" in mount, "cleanup_failed: ownership mount");
    if (mount.Type === "volume") {
      assert.ok("Name" in mount && typeof mount.Name === "string" && /^[a-f0-9]{64}$/.test(mount.Name),
        "cleanup_failed: ownership volume");
      ownedVolumes.push(mount.Name);
    }
  }
  const errors: string[] = [];
  const attempt = async (argv: string[]) => {
    try {
      const result = await command(argv);
      if (result.code !== 0) errors.push(`${argv[1]}: ${result.stderr}`);
      return result;
    } catch (error) {
      errors.push(`${argv[1]}: ${String(error)}`);
      return null;
    }
  };
  await attempt(["docker", "logs", id]);
  if (logDirectory) await attempt(["docker", "cp", `${id}:/logs/.`, logDirectory]);
  const stopped = state.Running ? await attempt(["docker", "stop", "--time", "30", id]) : null;
  if (!state.Running || stopped?.code === 0) {
    try {
      const inspection = await command(["docker", "inspect", id]);
      assert.equal(inspection.code, 0, "cleanup_failed: stopped inspection");
      const entries: unknown = JSON.parse(inspection.stdout);
      assert.ok(Array.isArray(entries) && entries.length === 1);
      const entry: unknown = entries[0];
      assert.ok(entry && typeof entry === "object" && "Id" in entry && entry.Id === id && "State" in entry);
      const finalState = entry.State;
      assert.ok(finalState && typeof finalState === "object" && "Running" in finalState
        && "OOMKilled" in finalState && "ExitCode" in finalState);
      if (finalState.Running !== false) errors.push("container did not stop");
      if (finalState.OOMKilled !== false) errors.push("resource_failed: OOM state");
      if (finalState.ExitCode !== 0) errors.push(`container exit: ${String(finalState.ExitCode)}`);
    } catch (error) { errors.push(`stopped inspection: ${String(error)}`); }
  }
  await attempt(["docker", "rm", "-f", "-v", id]);
  const remaining = await attempt(["docker", "container", "ls", "-a", "--filter", `id=${id}`, "--format", "{{.ID}}"]);
  const containerAbsent = remaining?.code === 0 && remaining.stdout.trim() === "";
  let volumesAbsent = true;
  for (const volume of ownedVolumes) {
    const result = await attempt(["docker", "volume", "ls", "--filter", `name=^${volume}$`, "--format", "{{.Name}}"]);
    if (result?.code !== 0 || result.stdout.trim() !== "") volumesAbsent = false;
  }
  if (!containerAbsent) errors.push("container remains or absence is unverified");
  if (!volumesAbsent) errors.push("volumes remain or absence is unverified");
  return { containerAbsent, volumesAbsent, cid: id, ownedVolumes, errors };
}

class Recorder {
  sequence = 0;
  constructor(readonly directory: string, readonly secrets: string[]) {}
  redact(text: string) { for (const secret of this.secrets) text = text.replaceAll(secret, "<redacted>"); return text; }
  async json(name: string, value: unknown) { await writeFile(join(this.directory, name), this.redact(JSON.stringify(value, null, 2)) + "\n"); }
  async command(argv: string[], stdin = "", allowFailure = false): Promise<CommandResult> {
    const prefix = `${String(++this.sequence).padStart(4, "0")}-${argv[0]!.replaceAll(/[^a-zA-Z0-9_-]/g, "_")}`;
    const started = new Date().toISOString();
    await this.json(`${prefix}.command.json`, { argv, cwd: ROOT, started, stdinSha256: sha256(stdin), timeoutMs: 120000 });
    if (stdin) await writeFile(join(this.directory, `${prefix}.stdin`), this.redact(stdin));
    const result = await new Promise<CommandResult>((resolveResult, reject) => {
      const child = spawn(argv[0]!, argv.slice(1), { cwd: ROOT, env: { PATH: process.env.PATH!, HOME: process.env.HOME!, DOCKER_HOST: "unix:///Users/ino/.colima/default/docker.sock" }, stdio: "pipe" });
      let stdout = "", stderr = "", timedOut = false;
      const timeout = setTimeout(() => { timedOut = true; child.kill("SIGKILL"); }, 120000);
      child.stdout.on("data", bytes => { stdout += bytes; }); child.stderr.on("data", bytes => { stderr += bytes; });
      child.once("error", error => { clearTimeout(timeout); reject(error); });
      child.once("close", (code, signal) => { clearTimeout(timeout); resolveResult({ code, signal, stdout, stderr: stderr + (timedOut ? "\ncommand deadline exceeded" : "") }); });
      child.stdin.on("error", error => { stderr += `\nstdin: ${error.message}`; }); child.stdin.end(stdin);
    });
    await writeFile(join(this.directory, `${prefix}.stdout`), this.redact(result.stdout));
    await writeFile(join(this.directory, `${prefix}.stderr`), this.redact(result.stderr));
    await this.json(`${prefix}.exit.json`, { code: result.code, signal: result.signal, completed: new Date().toISOString() });
    if (!allowFailure) assert.equal(result.code, 0, `command_failed: ${argv[0]} ${argv[1]}: ${this.redact(result.stderr)}`);
    return result;
  }
}

const sourceFiles = ["scripts/qa/g004-gds-solver-20.fixtures.ts", "scripts/qa/g004-gds-solver-20.runner.ts", "scripts/qa/g004-gds-solver-20.test.ts", "scripts/qa/g004-ppr-oracle.ts", "packages/core/src/dynamics/ppr.ts", "docs/07-gds-validation.md", ".omo/evidence/g004-gds-preflight/README.md"];
async function sourcePins() { return Object.fromEntries(await Promise.all(sourceFiles.map(async path => [path, sha256(await readFile(join(ROOT, path)))]))); }
const literal = (value: unknown): string => {
  if (typeof value === "number") { assert.ok(Number.isFinite(value)); return String(value); }
  if (typeof value === "string") { assert.ok(/^[a-zA-Z0-9_-]+$/.test(value)); return `'${value}'`; }
  if (Array.isArray(value)) return `[${value.map(literal).join(",")}]`;
  assert.ok(value && typeof value === "object");
  return `{${Object.entries(value).map(([key, v]) => { assert.ok(/^[a-zA-Z]+$/.test(key)); return `${key}:${literal(v)}`; }).join(",")}}`;
};

export async function runQualification(manifestPath = process.env.GDS_BENCH_ARTIFACT_MANIFEST) {
  await mkdir(EVIDENCE, { recursive: true });
  const runId = `run-${new Date().toISOString().replaceAll(/[:.]/g, "-")}-${randomUUID()}`;
  const containerName = `anamnesis-gds-${runId}`;
  const directory = join(EVIDENCE, runId); await mkdir(directory, { mode: 0o700 });
  const password = randomBytes(32).toString("hex"), recorder = new Recorder(directory, [password]);
  const startPins = await sourcePins(), results: { id: string; hash: string; inputHash: string }[] = [];
  let privateRoot: string | undefined, cid: string | undefined, ownedVolumes: string[] = [], failure: unknown, cleanupFailure: unknown;
  let attachment: ReturnType<typeof spawn> | undefined, completion: Promise<{ code: number | null; signal: NodeJS.Signals | null }> | undefined;
  let attachedOutput = "", attachedError = "";
  let createAttempted = false;
  let privateRootRemoved = false;
  let cleanupResult: Awaited<ReturnType<typeof cleanupGdsContainer>> | undefined;
  const inspect = async () => JSON.parse((await recorder.command(["docker", "inspect", cid!])).stdout)[0];
  await recorder.json("manifest.json", { runId, qualified: false, scope: "offline-solver-only", sourcePins: startPins, argv: process.argv, cwd: process.cwd(), runtime: { bun: Bun.version, node: process.version, arch: process.arch, platform: process.platform }, limits: LIMITS, config: CONFIG, configSha256: sha256(CONFIG.join("\n")), pins: BINARY_PINS, projection: PROJECTION, algorithm: ALGORITHM, manifestPath });
  try {
    const admission = await admitArtifact(manifestPath);
    assert.equal(Bun.version, BINARY_PINS.runtime, "version_mismatch: Bun");
    assert.equal(await realpath(process.execPath), await realpath(BINARY_PINS.runtimePath), "version_mismatch: runtime path");
    await recorder.json("admission.json", admission);
    const fixtures = generateFixtures(); assert.equal(JSON.stringify(fixtures), JSON.stringify(generateFixtures()));
    for (const f of fixtures) { validateInput(f.expectedCsr, new Map(f.rawSeeds), f.roleWeights); augmentedEdges(f); assert.deepEqual(exportFixedCsr(f), f.expectedCsr, "input_invalid: exported CSR differs from fixture rows"); }
    await recorder.json("fixture-index.json", fixtures.map(f => ({ id: f.id, seed: f.seed, hash: sha256(JSON.stringify(f)) })));
    const image = JSON.parse((await recorder.command(["docker", "image", "inspect", BINARY_PINS.image])).stdout)[0];
    assert.ok(image.RepoDigests.includes(BINARY_PINS.image), "version_mismatch: cached image digest");
    const platformImage = JSON.parse((await recorder.command(["docker", "image", "inspect", "--platform", BINARY_PINS.platform, BINARY_PINS.image])).stdout)[0];
    assert.equal(platformImage.Descriptor.digest, BINARY_PINS.platformManifest, "version_mismatch: platform manifest");
    assert.equal(platformImage.Architecture, "arm64"); assert.equal(platformImage.Os, "linux");
    assert.equal(platformImage.Variant, "v8");
    await recorder.json("image-identity.json", { image, platformImage,
      identityBasis: "Pinned repository digest and selected platform manifest; Docker Id is not the OCI config digest." });
    const info = JSON.parse((await recorder.command(["docker", "info", "--format", "{{json .}}"])).stdout);
    await recorder.json("resources.json", { NCPU: info.NCPU, MemTotal: info.MemTotal, boundedSerialConcurrency: 1, heapMiB: 512, pagecacheMiB: 128 });
    assert.ok(info.NCPU >= 1 && info.MemTotal >= 1024 * 1024 * 1024, "resource_failed: VM headroom");
    // The Docker VM shares the worktree, not macOS's /var/folders temp tree.
    privateRoot = await mkdtemp(join(directory, "private-")); await chmod(privateRoot, 0o700);
    const plugins = join(privateRoot, "plugins"); await mkdir(plugins, { mode: 0o755 });
    const plugin = join(plugins, "neo4j-graph-data-science-2.13.12.jar"); await copyFile(admission.jar.path, plugin); await chmod(plugin, 0o444);
    assert.equal(sha256(await readFile(plugin)), admission.jar.sha256);
    await writeFile(join(privateRoot, "neo4j.env"), `NEO4J_AUTH=neo4j/${password}\n${CONFIG.join("\n")}\n`, { mode: 0o600 });
    await writeFile(join(privateRoot, "shell.env"), `NEO4J_PASSWORD=${password}\n`, { mode: 0o600 });
    createAttempted = true;
    await appendFile(join(privateRoot, "neo4j.env"), "NEO4J_DEBUG=true\n");
    const created = await recorder.command(["docker", "create", "--name", containerName, "--hostname", "neo4j-gds", "--add-host", "neo4j-gds:127.0.0.1", "--pull", "never", "--platform", BINARY_PINS.platform, "--network", "none", "--restart", "no", "--no-healthcheck", "--label", "omo.task=gds-solver-20", "--label", `omo.owner=${runId}`, "--env-file", join(privateRoot, "neo4j.env"), "--mount", `type=bind,src=${plugins},dst=/plugins,readonly`, BINARY_PINS.image]);
    cid = created.stdout.trim(); assert.match(cid, /^[a-f0-9]{64}$/);
    const createdInspect = await inspect();
    ownedVolumes = createdInspect.Mounts.filter((m: { Type: string }) => m.Type === "volume").map((m: { Name: string }) => m.Name);
    assert.equal(createdInspect.Config.Labels["omo.owner"], runId);
    assert.equal(createdInspect.Config.Image, BINARY_PINS.image);
    assert.equal(createdInspect.Image, image.Id, "version_mismatch: created image identity");
    assert.equal(createdInspect.HostConfig.NetworkMode, "none"); assert.deepEqual(createdInspect.HostConfig.PortBindings ?? {}, {});
    assert.equal(createdInspect.HostConfig.Privileged, false); assert.equal(createdInspect.HostConfig.PidMode, "");
    assert.deepEqual(Object.keys(createdInspect.NetworkSettings.Networks), ["none"]);
    assert.equal(createdInspect.HostConfig.ExtraHosts.includes("neo4j-gds:127.0.0.1"), true);
    assert.equal(createdInspect.Mounts.length, 3); assert.equal(ownedVolumes.length, 2);
    assert.deepEqual(createdInspect.Mounts.filter((m: { Type: string }) => m.Type === "volume").map((m: { Destination: string }) => m.Destination).sort(), ["/data", "/logs"]);
    const mount = createdInspect.Mounts.find((m: { Destination: string }) => m.Destination === "/plugins"); assert.equal(mount.Source, plugins); assert.equal(mount.RW, false);
    await recorder.json("ownership.json", { runId, cid, ownedVolumes, privateRoot });
    // The log event is subscribed before docker start triggers server startup.
    const ready = new Promise<void>((readyResolve, readyReject) => {
      attachment = spawn("docker", ["start", "--attach", cid!], { cwd: ROOT, env: { PATH: process.env.PATH!, HOME: process.env.HOME!, DOCKER_HOST: "unix:///Users/ino/.colima/default/docker.sock" }, stdio: ["ignore", "pipe", "pipe"] });
      const timer = setTimeout(() => readyReject(new Error("resource_failed: startup deadline 120000ms")), 120000);
      completion = new Promise(resolveExit => {
        attachment!.once("close", (code, signal) => { clearTimeout(timer); resolveExit({ code, signal }); readyReject(new Error(`resource_failed: startup exit ${code}/${signal}`)); });
      });
      attachment.once("error", error => { clearTimeout(timer); readyReject(error); });
      attachment.stdout!.on("data", bytes => { attachedOutput += bytes; if (/(?:^|\n).*\bINFO\s+Started\.\s*(?:\r?\n)/.test(attachedOutput)) { clearTimeout(timer); readyResolve(); } });
      attachment.stderr!.on("data", bytes => { attachedError += bytes; });
    });
    await recorder.json("start-attach.command.json", { argv: ["docker", "start", "--attach", cid], cwd: ROOT, readiness: "Neo4j startup-complete log event", timeoutMs: 120000 });
    await ready;
    const exec = (args: string[]) => recorder.command(["docker", "exec", cid!, ...args]);
    await exec(["java", "-version"]); await exec(["uname", "-m"]); await exec(["cypher-shell", "--version"]);
    // This pinned cypher-shell prints usage with exit 1 even for --help.
    const help = await recorder.command(["docker", "exec", cid!, "cypher-shell", "--help"], "", true);
    assert.ok((help.code === 0 || help.code === 1) && help.signal === null && help.stderr === ""
      && help.stdout.startsWith("usage: cypher-shell ") && help.stdout.includes("--fail-fast"), "transport_invalid: cypher-shell help");
    const network = (await exec(["cat", "/proc/net/dev", "/proc/net/route"])).stdout;
    assert.deepEqual([...network.matchAll(/^\s*(\w+):/gm)].map(match => match[1]), ["lo"]);
    assert.ok(!network.split("Iface")[1]!.trim().split("\n").slice(1).some(line => line.trim()), "resource_failed: external route");
    const query = async (cypher: string, header: string[], graph?: string) => {
      const stdin = (graph ? `:param graph => '${graph}'\n` : "") + cypher + ";\n";
      const result = await recorder.command(["docker", "exec", "-i", "--env-file", join(privateRoot!, "shell.env"), cid!, "cypher-shell", "-a", "bolt://127.0.0.1:7687", "-u", "neo4j", "-d", "neo4j", "--format", "plain", "--fail-fast"], stdin);
      return parseCsv(result.stdout, header);
    };
    assert.deepEqual(await query("RETURN 1 AS authenticated", ["authenticated"]), [["1"]]);
    const components = await query("CALL dbms.components() YIELD versions, edition RETURN versions[0] AS version, edition", ["version", "edition"]);
    assert.deepEqual(components, [[BINARY_PINS.neo4jVersion, "community"]], "version_mismatch: Neo4j");
    const functions = await query("SHOW FUNCTIONS YIELD name, signature WHERE name IN ['gds.version','gds.util.asNode'] RETURN name, signature ORDER BY name", ["name", "signature"]);
    assert.equal(functions.length, 2, "version_mismatch: GDS functions unavailable");
    assert.deepEqual(await query("RETURN gds.version() AS version", ["version"]), [[BINARY_PINS.gdsVersion]], "version_mismatch: GDS");
    const procedures = await query("SHOW PROCEDURES YIELD name, signature WHERE name STARTS WITH 'gds.' RETURN name, signature ORDER BY name", ["name", "signature"]);
    for (const name of ["gds.graph.project", "gds.graph.relationshipProperty.stream", "gds.pageRank.stream", "gds.graph.drop", "gds.graph.list", "gds.debug.sysInfo"]) assert.ok(procedures.some(row => row[0] === name), `version_mismatch: missing ${name}`);
    const system = await query("CALL gds.debug.sysInfo() YIELD key, value RETURN key, toString(value) AS value ORDER BY key", ["key", "value"]);
    const license = system.find(row => row[0] === "gdsEdition");
    assert.ok(license && typeof license[1] === "string", "version_mismatch: missing GDS edition");
    const licensed = await query("RETURN gds.isLicensed() AS licensed", ["licensed"]);
    assert.deepEqual(licensed, [["FALSE"]], "version_mismatch: no Enterprise license expected");
    const edition = validateGdsEdition(license[1], false);
    await recorder.json("versions-catalog.json", { components, functions, procedures, system, licensed, edition });
    for (const f of fixtures) {
      const graph = `solver_${f.id}`, caseDir = join(directory, f.id); await mkdir(caseDir);
      const save = async (name: string, data: unknown) => writeFile(join(caseDir, name), JSON.stringify(data, null, 2) + "\n");
      const inputHash = sha256(JSON.stringify(f)); await save("input.json", f);
      assert.deepEqual(await query("CALL gds.graph.list() YIELD graphName RETURN graphName", ["graphName"]), [], "projection_mismatch: catalog not empty");
      const edges = augmentedEdges(f); await save("augmented-edges.json", edges);
      const cyphers: string[] = [];
      const q = async (cypher: string, header: string[]) => { cyphers.push(cypher); return query(cypher, header, graph); };
      assert.deepEqual(await q(`UNWIND ${literal([...f.nodes, "__sigma__"])} AS id CREATE (:GdsBench {id:id}) RETURN count(*) AS created`, ["created"]), [[String(f.nodes.length + 1)]]);
      // Bounded batches avoid shell/client line limits without changing edge multiplicity.
      for (let begin = 0; begin < edges.length; begin += 512) {
        const batch = edges.slice(begin, begin + 512);
        assert.deepEqual(await q(`UNWIND ${literal(batch)} AS edge MATCH (a:GdsBench {id:edge.from}), (b:GdsBench {id:edge.to}) CREATE (a)-[:TRANSITION {weight:edge.weight}]->(b) RETURN count(*) AS created`, ["created"]), [[String(batch.length)]]);
      }
      const projected = await q("CALL gds.graph.project($graph, 'GdsBench', {TRANSITION: {type: 'TRANSITION', orientation: 'NATURAL', aggregation: 'NONE', properties: {weight: {property: 'weight', aggregation: 'NONE'}}}}, {readConcurrency: 1, validateRelationships: true}) YIELD nodeCount, relationshipCount RETURN nodeCount, relationshipCount", ["nodeCount", "relationshipCount"]);
      assert.deepEqual(projected, [[String(f.nodes.length + 1), String(edges.length)]], "projection_mismatch: counts");
      const streamed = await q("CALL gds.graph.relationshipProperty.stream($graph, 'weight') YIELD sourceNodeId, targetNodeId, propertyValue RETURN gds.util.asNode(sourceNodeId).id AS source, gds.util.asNode(targetNodeId).id AS target, propertyValue AS weight ORDER BY source, target, weight", ["source", "target", "weight"]);
      await save("projection.json", { projected, rawStream: streamed });
      checkProjection(edges, streamed.map(row => ({ from: row[0]!, to: row[1]!, weight: numeric(row[2]!) })));
      const raw = await q(`MATCH (sigma:GdsBench {id: '__sigma__'}) CALL gds.pageRank.stream($graph, {sourceNodes: [sigma], dampingFactor: ${ALGORITHM.dampingFactor}, relationshipWeightProperty: 'weight', concurrency: ${ALGORITHM.concurrency}, tolerance: ${ALGORITHM.tolerance}, maxIterations: ${ALGORITHM.maxIterations}}) YIELD nodeId, score RETURN gds.util.asNode(nodeId).id AS id, score ORDER BY id ASC`, ["id", "score"]);
      await save("gds-raw.json", raw);
      const gds = normalizedScores(f, raw), csr = exportFixedCsr(f); assert.deepEqual(csr, f.expectedCsr);
      const local = solveFixedCsr(csr, new Map(f.rawSeeds), { alpha: 0.85, tolerance: 1e-4, maxIter: 64, roleWeights: f.roleWeights });
      const rGds = residual(f, gds.values), rTs = residual(f, local.values);
      await save("numeric-before-gates.json", { gdsExecuted: true, gds, local: { ...local, values: [...local.values] }, rGds, rTs });
      assert.ok(rGds <= 1e-8, `reference_residual_failed: ${f.id}: ${rGds}`);
      assert.ok(local.iterations <= 64 && local.iterateDeltaL1 < 1e-4, `solver_failed: ${f.id}: convergence`);
      assert.ok(Math.abs(rTs - local.residualL1) <= 1e-12, "solver_failed: independent residual");
      const comparison = compareScores(f.nodes, local.values, gds.values, rTs, rGds);
      const topK = comparison.topK.map(item => ({ ...item, cutoff: [...gds.values].sort((a, b) => b - a)[item.effectiveK - 1], tieBreak: "ID_ASC", linearGain: "p_gds" }));
      const analytic = analyticValues(f);
      if (analytic) assert.ok(analytic.reduce((sum, v, i) => sum + Math.abs(v - gds.values[i]!), 0) <= 1e-8 / 0.15 + 1e-12, "reference_residual_failed: analytic");
      const dense = f.seed <= 7 ? exactReference(csr, new Map(f.rawSeeds), f.roleWeights) : null;
      if (dense) assert.ok(dense.values.reduce((sum, v, i) => sum + Math.abs(v - gds.values[i]!), 0) <= 1e-8 / 0.15 + 1e-12, "reference_residual_failed: dense cross-check");
      const result = { id: f.id, seed: f.seed, inputHash, passed: true, gdsExecuted: true, pins: BINARY_PINS, csr, gds, local: { ...local, values: [...local.values] }, residuals: { gds: rGds, ts: rTs }, bounds: { gds: rGds / 0.15, tsResidual: rTs / 0.15, tsDelta: local.errorBoundL1, referenceMax: 1e-8 / 0.15 }, massErrors: { gds: Math.abs(gds.values.reduce((a, b) => a + b, 0) - 1), ts: Math.abs(local.mass - 1) }, ...comparison, topK, analytic, dense: dense?.values ?? null, cypherSha256: sha256(cyphers.join("\n")) };
      await save("cypher.json", cyphers); await save("result.json", result);
      const resultBytes = await readFile(join(caseDir, "result.json")); assert.deepEqual(JSON.parse(resultBytes.toString()), result);
      results.push({ id: f.id, inputHash, hash: sha256(resultBytes) });
      await q("CALL gds.graph.drop($graph) YIELD graphName RETURN graphName", ["graphName"]);
      assert.deepEqual(await q(`MATCH (n:GdsBench) WHERE n.id STARTS WITH '${f.id}-' OR n.id = '__sigma__' DETACH DELETE n RETURN count(*) AS deleted`, ["deleted"]), [[String(f.nodes.length + 1)]]);
      assert.deepEqual(await query("MATCH (n) RETURN count(n) AS remaining", ["remaining"]), [["0"]]);
      console.log(JSON.stringify({ event: "gds-case-passed", id: f.id, l1: comparison.l1, residual: rGds }));
    }
    assert.equal(results.length, 20); assert.equal(new Set(results.map(r => r.inputHash)).size, 20);
    assert.deepEqual(await query("CALL gds.graph.list() YIELD graphName RETURN graphName", ["graphName"]), []);
  } catch (error) { failure = error; await recorder.json("failure.json", { message: String(error), stack: error instanceof Error ? error.stack : null }); }
  finally {
    try {
      if (createAttempted) {
        cleanupResult = await cleanupGdsContainer(argv => recorder.command(argv, "", true), containerName, runId, join(directory, "neo4j-logs"));
        cid = cleanupResult.cid ?? cid;
        ownedVolumes = cleanupResult.ownedVolumes;
        if (completion) {
          let timer: ReturnType<typeof setTimeout> | undefined;
          const ended = await Promise.race([completion, new Promise<never>((_, reject) => { timer = setTimeout(() => { attachment?.kill("SIGKILL"); reject(new Error("cleanup_failed: attach exit deadline")); }, 120000); })]).finally(() => clearTimeout(timer));
          await recorder.json("start-attach.exit.json", ended);
          assert.equal(ended.code, 0, "cleanup_failed: container attachment exit");
        }
        assert.deepEqual(cleanupResult.errors, [], "cleanup_failed: owned teardown diagnostics");
      }
    } catch (error) { cleanupFailure = error; await recorder.json("cleanup-failure.json", { message: String(error), cid, ownedVolumes }); }
    finally {
      try {
        await writeFile(join(directory, "start-attach.stdout"), recorder.redact(attachedOutput));
        await writeFile(join(directory, "start-attach.stderr"), recorder.redact(attachedError));
      } finally {
        // Keep a bind-mounted directory intact if container removal was not
        // verified. This path is retained as an unresolved owned resource.
        if (privateRoot && (!createAttempted || cleanupResult?.containerAbsent)) {
          await rm(privateRoot, { recursive: true, force: true });
          await assert.rejects(stat(privateRoot), { code: "ENOENT" });
          privateRootRemoved = true;
        }
      }
    }
  }
  const endPins = await sourcePins();
  if (JSON.stringify(startPins) !== JSON.stringify(endPins)) failure = new Error("solver_failed: source snapshot changed");
  await recorder.json("source-end.json", endPins);
  await recorder.json("cleanup.json", { passed: !cleanupFailure, cid: cid ?? null, containerName, ownedVolumes,
    containerAbsent: cleanupResult?.containerAbsent ?? !createAttempted, volumesAbsent: cleanupResult?.volumesAbsent ?? !createAttempted,
    errors: cleanupResult?.errors ?? [], privateRootRemoved, privateRoot: privateRoot ?? null, createAttempted });
  const summary = { qualified: !failure && !cleanupFailure && results.length === 20, scope: "offline-solver-only", gdsExecuted: results.length > 0, passed: results.length, results, runId, directory, failure: failure ? String(failure) : null, cleanupFailure: cleanupFailure ? String(cleanupFailure) : null };
  await recorder.json("summary.json", summary); assert.deepEqual(JSON.parse(await readFile(join(directory, "summary.json"), "utf8")), summary);
  if (cleanupFailure) throw new Error(`cleanup_failed: ${directory}`, { cause: cleanupFailure });
  if (failure) throw new Error(`qualification_failed: ${directory}: ${String(failure)}`, { cause: failure });
  assert.equal(summary.qualified, true); return summary;
}

if (import.meta.main) await runQualification();
