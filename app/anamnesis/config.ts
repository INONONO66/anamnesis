import { randomBytes, randomUUID } from "node:crypto";
import fs from "node:fs/promises";
import { homedir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { RpcIngestStatusParams } from "../../packages/protocol/src/rpc.ts";

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
