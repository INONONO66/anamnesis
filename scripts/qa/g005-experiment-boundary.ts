import { createHash } from "node:crypto";

export type ExperimentSpec = {
  name: string;
  command: string[];
  image: { reference: string; digest: string };
  source: { files: Array<{ path: string; sha256: string }> };
  input: { bytes: number; records: number };
  resources: { maxBytes: number; maxSeconds: number };
  expected: { artifact: string; sha256: string };
};
export type Execution = { output: string; artifacts: Record<string, string>; cleanup?: { owned: boolean; clean: boolean } };
export type Verification = { outcome: "pass" | "fail" | "unknown"; reason: string; artifact_sha256?: string; cleanup?: { owned: boolean; clean: boolean } | undefined };
export type Manifest = Readonly<ExperimentSpec & { schema: "g005-experiment/v1"; run_id: string; command_sha256: string; manifest_sha256: string; created_at: string }>;

type Runner = (command: readonly string[], limits: ExperimentSpec["resources"]) => Promise<Execution>;
const HEX = /^[a-f0-9]{64}$/;
const hash = (value: string) => createHash("sha256").update(value).digest("hex");
const canonical = (value: unknown): string => JSON.stringify(value, (_key, item) => item && typeof item === "object" && !Array.isArray(item)
  ? Object.fromEntries(Object.entries(item as Record<string, unknown>).sort(([a], [b]) => a.localeCompare(b))) : item);
const freeze = <T>(value: T): T => { if (value && typeof value === "object") { Object.freeze(value); for (const child of Object.values(value as object)) freeze(child); } return value; };

function validate(spec: ExperimentSpec): void {
  if (!spec.name || !Array.isArray(spec.command) || spec.command.length === 0 || !spec.command[0]!.startsWith("/")) throw new Error("command must be absolute and pinned");
  if (["/bin/sh", "/bin/bash", "/usr/bin/env", "/bin/zsh"].includes(spec.command[0]!)) throw new Error("unsafe shell command");
  if (!/^sha256:[a-f0-9]{64}$/.test(spec.image.digest) || !spec.image.reference.endsWith(`@${spec.image.digest}`)) throw new Error("image must be digest-pinned");
  if (spec.source.files.length === 0 || spec.source.files.some(file => !file.path || !HEX.test(file.sha256))) throw new Error("source files must be hashed");
  if (!Number.isSafeInteger(spec.input.bytes) || spec.input.bytes < 0 || !Number.isSafeInteger(spec.input.records) || spec.input.records < 0) throw new Error("invalid input bounds");
  if (!Number.isFinite(spec.resources.maxSeconds) || spec.resources.maxSeconds <= 0 || !Number.isSafeInteger(spec.resources.maxBytes) || spec.resources.maxBytes <= 0) throw new Error("invalid resource bounds");
  if (!HEX.test(spec.expected.sha256) || !spec.expected.artifact) throw new Error("expected artifact must be hashed");
}

export function createManifest(spec: ExperimentSpec, createdAt: string): Manifest {
  validate(spec);
  const command_sha256 = hash(canonical(spec.command));
  const base = { schema: "g005-experiment/v1" as const, ...structuredClone(spec), command_sha256, created_at: createdAt };
  const run_id = `run-${hash(canonical(base))}`;
  return freeze({ ...base, run_id, manifest_sha256: hash(canonical({ ...base, run_id })) });
}

export function verifyResult(manifest: Manifest, execution: Execution): Verification {
  const cleanup = execution.cleanup;
  const content = execution.artifacts[manifest.expected.artifact];
  if (content === undefined) return { outcome: "unknown", reason: "required result artifact missing", cleanup };
  if (Object.keys(execution.artifacts).length !== 1) return { outcome: "fail", reason: "result artifact set is partial or unexpected", cleanup };
  const artifact_sha256 = hash(content);
  if (artifact_sha256 !== manifest.expected.sha256) return { outcome: "fail", reason: "result artifact checksum mismatch", artifact_sha256, cleanup };
  if (!cleanup) return { outcome: "unknown", reason: "cleanup ownership verification missing" };
  if (!cleanup.owned || !cleanup.clean) return { outcome: "fail", reason: "cleanup ownership verification failed", cleanup };
  return { outcome: "pass", reason: "verified manifest, result checksum, and cleanup ownership", artifact_sha256, cleanup };
}

export async function executeExperiment(spec: ExperimentSpec, runner: Runner): Promise<Verification> {
  const manifest = createManifest(spec, "1970-01-01T00:00:00.000Z");
  const execution = await runner(manifest.command, manifest.resources);
  if (Buffer.byteLength(execution.output) > manifest.resources.maxBytes) return { outcome: "fail", reason: "output exceeded declared byte bound", cleanup: execution.cleanup };
  return verifyResult(manifest, execution);
}
