import { randomBytes, randomUUID } from "node:crypto";
import fs from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import type { ExtractionDialect } from "../../packages/core/src/openai-extraction-provider.ts";
import { RpcEmbeddingAttempt, RpcIngestStatusParams } from "../../packages/protocol/src/rpc.ts";

// Reuse the protocol's Zod schemas; the app has no independent Zod dependency.
const providerUrl = RpcEmbeddingAttempt.shape.model.refine(value => URL.canParse(value) && ["http:", "https:"].includes(new URL(value).protocol));
const providerName = RpcEmbeddingAttempt.shape.model.trim().min(1).max(256);
const providerSecret = RpcEmbeddingAttempt.shape.model.refine(value => !/[\r\n]/.test(value));
const providerPath = RpcEmbeddingAttempt.shape.model;
export const DEFAULT_EXTRACTION_PROMPT_FILE = fileURLToPath(new URL("./prompts/extract-claims.v2.md", import.meta.url));
export const DEFAULT_RELATION_PROMPT_FILE = fileURLToPath(new URL("./prompts/judge-relations.v1.md", import.meta.url));
const ProviderEnvironment = RpcEmbeddingAttempt.pick({}).strip().extend({
  ANAMNESIS_EMBEDDING_BASE_URL: providerUrl.optional(),
  ANAMNESIS_EMBEDDING_MODEL: providerName.default("Qwen3-Embedding-0.6B"),
  ANAMNESIS_EMBEDDING_DIMENSIONS: RpcEmbeddingAttempt.shape.dimensions.default(1024),
  ANAMNESIS_EMBEDDING_API_KEY: providerSecret.optional(),
  ANAMNESIS_LLM_BASE_URL: providerUrl.optional(),
  ANAMNESIS_LLM_API_KEY_FILE: providerPath.optional(),
  ANAMNESIS_LLM_MODEL: providerName.default("claude-haiku-4-5"),
  ANAMNESIS_LLM_DIALECT: providerName.refine(value => value === "openai_chat" || value === "anthropic_messages").transform(value => value as ExtractionDialect).optional(),
  ANAMNESIS_EXTRACTION_PROMPT_FILE: providerPath.default(DEFAULT_EXTRACTION_PROMPT_FILE),
  ANAMNESIS_RELATION_PROMPT_FILE: providerPath.default(DEFAULT_RELATION_PROMPT_FILE),
});

/** Load once at provider startup. Absent base URLs leave legacy provider selection unchanged.
 * Secrets are never included in validation errors; callers must not log this result. */
export async function loadProviderConfig(env: NodeJS.ProcessEnv = process.env) {
  const embeddingEnabled = env["ANAMNESIS_EMBEDDING_BASE_URL"] !== undefined;
  const parsed = ProviderEnvironment.safeParse({ ...env,
    ANAMNESIS_EMBEDDING_MODEL: embeddingEnabled ? env["ANAMNESIS_EMBEDDING_MODEL"] : undefined,
    ANAMNESIS_EMBEDDING_API_KEY: embeddingEnabled ? env["ANAMNESIS_EMBEDDING_API_KEY"] : undefined,
    ANAMNESIS_EMBEDDING_DIMENSIONS: !embeddingEnabled || env["ANAMNESIS_EMBEDDING_DIMENSIONS"] === undefined ? undefined : Number(env["ANAMNESIS_EMBEDDING_DIMENSIONS"]),
  });
  if (!parsed.success) throw new Error(`invalid provider configuration: ${parsed.error.issues.map(issue => issue.path.join(".")).join(", ")}`);
  const config = parsed.data;
  let apiKey: string | undefined;
  if (config.ANAMNESIS_LLM_API_KEY_FILE !== undefined) {
    let value: unknown;
    try { value = JSON.parse(await fs.readFile(config.ANAMNESIS_LLM_API_KEY_FILE, "utf8")); }
    catch { throw new Error("unable to read ANAMNESIS_LLM_API_KEY_FILE as JSON"); }
    const key = RpcEmbeddingAttempt.pick({}).strip().extend({ bearer: providerSecret }).safeParse(value);
    if (!key.success) throw new Error("invalid bearer in ANAMNESIS_LLM_API_KEY_FILE");
    apiKey = key.data.bearer;
  }
  const promptFile = resolve(config.ANAMNESIS_EXTRACTION_PROMPT_FILE);
  const systemPrompt = await fs.readFile(promptFile, "utf8");
  if (!systemPrompt.trim()) throw new Error("empty ANAMNESIS_EXTRACTION_PROMPT_FILE");
  const relationPromptFile = resolve(config.ANAMNESIS_RELATION_PROMPT_FILE);
  const relationPrompt = await fs.readFile(relationPromptFile, "utf8");
  if (!relationPrompt.trim()) throw new Error("empty ANAMNESIS_RELATION_PROMPT_FILE");
  return {
    embedding: config.ANAMNESIS_EMBEDDING_BASE_URL === undefined ? undefined : {
      baseUrl: config.ANAMNESIS_EMBEDDING_BASE_URL,
      model: config.ANAMNESIS_EMBEDDING_MODEL,
      dimensions: config.ANAMNESIS_EMBEDDING_DIMENSIONS,
      ...(config.ANAMNESIS_EMBEDDING_API_KEY === undefined ? {} : { apiKey: config.ANAMNESIS_EMBEDDING_API_KEY }),
    },
    llm: { baseUrl: config.ANAMNESIS_LLM_BASE_URL, model: config.ANAMNESIS_LLM_MODEL,
      dialect: config.ANAMNESIS_LLM_DIALECT ?? (config.ANAMNESIS_LLM_MODEL.startsWith("claude") ? "anthropic_messages" : "openai_chat"),
      ...(apiKey === undefined ? {} : { apiKey }) },
    promptFile, systemPrompt, relationPromptFile, relationPrompt,
  };
}

export function hasCode(error: unknown, code: string): boolean {
  return error instanceof Error && "code" in error && error.code === code;
}
export async function syncDirectory(path: string): Promise<void> {
  const file = await fs.open(path, "r");
  try { await file.sync(); } finally { await file.close(); }
}
export async function atomicJson(path: string, value: unknown): Promise<void> {
  const temporary = `${path}.${randomUUID()}.tmp`;
  const file = await fs.open(temporary, "wx", 0o600).catch(async (error: unknown) => {
    // An I/O failure may leave a created path, but EEXIST is not ours to remove.
    if (!hasCode(error, "EEXIST")) await fs.rm(temporary, { force: true });
    throw error;
  });
  try {
    try {
      await file.writeFile(JSON.stringify(value) + "\n");
      await file.sync();
    } finally { await file.close(); }
    await fs.rename(temporary, path);
    await syncDirectory(dirname(path));
  } finally { await fs.rm(temporary, { force: true }); }
}
export function runtimeRoot(): string {
  return resolve(process.env["ANAMNESIS_RUNTIME_ROOT"] ?? join(homedir(), ".anamnesis"));
}
export function socketPath(root: string): string {
  const path = join(root, "anamnesis.sock");
  if (Buffer.byteLength(path) > 103) throw new Error("runtime root exceeds portable Unix socket path limit");
  return path;
}
const Owner = { parse(value: unknown): { pid: number; nonce: string } {
  if (!value || typeof value !== "object" || !("pid" in value) || !("nonce" in value) || typeof value.pid !== "number" || !Number.isSafeInteger(value.pid) || value.pid < 1 || typeof value.nonce !== "string") throw new Error("invalid owner lease");
  return { pid: value.pid, nonce: value.nonce };
} };
export interface Installation {
  root: string;
  token: string;
  incarnation: string;
  epoch: string;
  assertOwned(): Promise<void>;
  release(): Promise<void>;
}

/** A live PID is conservatively protected, including PID reuse. No timed lease theft. */
export async function acquireInstallation(root: string): Promise<Installation> {
  await fs.mkdir(root, { recursive: true, mode: 0o700 });
  const info = await fs.lstat(root);
  if (!info.isDirectory() || info.isSymbolicLink() || info.uid !== process.getuid?.()) {
    throw new Error("runtime root must be a directory owned by the current user");
  }
  await fs.chmod(root, 0o700);
  const lease = join(root, "owner");
  const ownerPath = join(lease, "owner.json");
  const epoch = randomUUID();
  const claim = async () => {
    await fs.mkdir(lease, { mode: 0o700 });
    await atomicJson(ownerPath, { pid: process.pid, nonce: epoch });
    await syncDirectory(root);
  };
  try { await claim(); }
  catch (error) {
    if (!hasCode(error, "EEXIST")) throw error;
    // Serialize dead-owner reclamation. An incomplete claim/reclamation is an
    // explicit ops error, never permission to steal a potentially live owner.
    const recovery = join(root, "owner-recovery");
    await fs.mkdir(recovery, { mode: 0o700 });
    try {
      const owner = Owner.parse(JSON.parse(await fs.readFile(ownerPath, "utf8")));
      let dead = false;
      try { process.kill(owner.pid, 0); }
      catch (cause) { if (hasCode(cause, "ESRCH")) dead = true; else throw cause; }
      if (!dead) throw new Error(`runtime root is owned by live pid ${owner.pid}`);
      await fs.rm(lease, { recursive: true });
      await claim();
    } finally { await fs.rm(recovery, { recursive: true }); }
  }
  const assertOwned = async () => {
    const owner = Owner.parse(JSON.parse(await fs.readFile(ownerPath, "utf8")));
    if (owner.pid !== process.pid || owner.nonce !== epoch) throw new Error("ownership_lost");
  };
  const release = async () => { await assertOwned(); await fs.rm(lease, { recursive: true }); await syncDirectory(root); };
  try {
    const tokenPath = join(root, "token");
    let token: string;
    try { token = (await fs.readFile(tokenPath, "utf8")).trim(); }
    catch (error) {
      if (!hasCode(error, "ENOENT")) throw error;
      token = process.env["ANAMNESIS_RUNTIME_TOKEN"] ?? randomBytes(32).toString("base64url");
      const file = await fs.open(tokenPath, "wx", 0o600);
      try { await file.writeFile(token + "\n"); await file.sync(); } finally { await file.close(); }
      await syncDirectory(root);
    }
    if (!token || Buffer.byteLength(token) > 1024) throw new Error("invalid installation token");
    if (process.env["ANAMNESIS_RUNTIME_TOKEN"] && process.env["ANAMNESIS_RUNTIME_TOKEN"] !== token) {
      throw new Error("configured token differs from the persisted installation token");
    }
    await fs.chmod(tokenPath, 0o600);
    const incarnationPath = join(root, "incarnation.json");
    let incarnation: string;
    try { incarnation = RpcIngestStatusParams.shape.data_incarnation.parse(JSON.parse(await fs.readFile(incarnationPath, "utf8"))); }
    catch (error) {
      if (!hasCode(error, "ENOENT")) throw error;
      incarnation = randomUUID();
      await atomicJson(incarnationPath, incarnation);
    }
    return { root, token, incarnation, epoch, assertOwned, release };
  } catch (error) { await release(); throw error; }
}
