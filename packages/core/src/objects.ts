// Use the shared builtin objects: Bun does not synchronize patched default
// exports into namespace imports via syncBuiltinESMExports().
import crypto from "node:crypto";
import fs from "node:fs/promises";
import { join } from "node:path";

const HASH_PATTERN = /^[0-9a-f]{64}$/;

export interface ObjectMetadata { hash: string; size: number; mediaType: string; }

export class ObjectStoreError extends Error {
  readonly code: "invalid-hash" = "invalid-hash";
  constructor(hash: string) { super(`Invalid object hash: ${hash}`); this.name = "ObjectStoreError"; }
}

export class ObjectStore {
  constructor(private readonly root: string) {}

  async put(bytes: Uint8Array, mediaType: string): Promise<ObjectMetadata> {
    const hash = crypto.createHash("sha256").update(bytes).digest("hex");
    const metadata = { hash, size: bytes.byteLength, mediaType };
    const path = this.objectPath(hash), directory = join(this.root, hash.slice(0, 2));
    await fs.mkdir(directory, { recursive: true, mode: 0o700 });
    if (await this.has(hash)) return metadata;
    const existing = await fs.readFile(path).catch((error: unknown) => {
      if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error;
      return undefined;
    });
    if (existing !== undefined) {
      if (crypto.createHash("sha256").update(existing).digest("hex") !== hash) throw new Error("object content hash mismatch");
      if (existing.byteLength !== bytes.byteLength) throw new Error("object size mismatch");
      await this.publishMetadata(metadata);
      return metadata;
    }
    const temporaryPath = join(directory, `.${hash}.${crypto.randomUUID()}.tmp`);
    const file = await fs.open(temporaryPath, "wx", 0o600);
    let published = false;
    try {
      try { await file.writeFile(bytes); await file.sync(); }
      finally { await file.close(); }
      if (await this.has(hash)) return metadata;
      await fs.rename(temporaryPath, path); published = true;
      await this.syncDirectory(directory);
      await this.publishMetadata(metadata);
      return metadata;
    } finally { if (!published) await fs.unlink(temporaryPath); }
  }

  async get(hash: string): Promise<Uint8Array> { return new Uint8Array(await fs.readFile(this.objectPath(hash))); }

  async has(hash: string): Promise<boolean> {
    const path = this.objectPath(hash);
    try {
      const metadata = JSON.parse(await fs.readFile(this.metadataPath(hash), "utf8")) as ObjectMetadata;
      const info = await fs.stat(path);
      if (metadata.hash !== hash || metadata.size !== info.size || metadata.size < 0 || typeof metadata.mediaType !== "string") return false;
      const bytes = await fs.readFile(path);
      return crypto.createHash("sha256").update(bytes).digest("hex") === hash;
    } catch (error) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") return false;
      throw error;
    }
  }

  private metadataPath(hash: string): string { return `${this.objectPath(hash)}.json`; }
  private async publishMetadata(metadata: ObjectMetadata): Promise<void> {
    const path = this.metadataPath(metadata.hash), temporaryPath = `${path}.${crypto.randomUUID()}.tmp`;
    const file = await fs.open(temporaryPath, "wx", 0o600); let published = false;
    try {
      try { await file.writeFile(JSON.stringify(metadata)); await file.sync(); }
      finally { await file.close(); }
      await fs.rename(temporaryPath, path); published = true;
      await this.syncDirectory(join(this.root, metadata.hash.slice(0, 2)));
    } finally { if (!published) await fs.unlink(temporaryPath); }
  }
  async verifyMissing(hashes: readonly string[]): Promise<string[]> {
    const missing: string[] = []; for (const hash of hashes) if (!(await this.has(hash))) missing.push(hash); return missing;
  }
  private objectPath(hash: string): string { if (!HASH_PATTERN.test(hash)) throw new ObjectStoreError(hash); return join(this.root, hash.slice(0, 2), hash); }
  private async syncDirectory(directory: string): Promise<void> {
    const handle = await fs.open(directory, "r"); try { await handle.sync(); } finally { await handle.close(); }
  }
}
