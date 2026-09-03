import { createHash, randomUUID } from "node:crypto";
import { mkdir, open, readFile, rename, stat, unlink } from "node:fs/promises";
import { join } from "node:path";

const HASH_PATTERN = /^[0-9a-f]{64}$/;

export interface ObjectMetadata {
  hash: string;
  size: number;
  mediaType: string;
}

export class ObjectStoreError extends Error {
  readonly code: "invalid-hash" = "invalid-hash";

  constructor(hash: string) {
    super(`Invalid object hash: ${hash}`);
    this.name = "ObjectStoreError";
  }
}

export class ObjectStore {
  constructor(private readonly root: string) {}

  async put(bytes: Uint8Array, mediaType: string): Promise<ObjectMetadata> {
    const hash = createHash("sha256").update(bytes).digest("hex");
    const metadata = { hash, size: bytes.byteLength, mediaType };
    const path = this.objectPath(hash);
    const directory = join(this.root, hash.slice(0, 2));

    await mkdir(directory, { recursive: true });
    if (await this.has(hash)) return metadata;

    const temporaryPath = join(directory, `.${hash}.${randomUUID()}.tmp`);
    const file = await open(temporaryPath, "wx", 0o600);
    try {
      await file.writeFile(bytes);
      await file.sync();
    } finally {
      await file.close();
    }

    let published = false;
    try {
      if (await this.has(hash)) return metadata;
      await rename(temporaryPath, path);
      published = true;
      await this.syncDirectory(directory);
      return metadata;
    } finally {
      if (!published) await unlink(temporaryPath);
    }
  }

  async get(hash: string): Promise<Uint8Array> {
    return new Uint8Array(await readFile(this.objectPath(hash)));
  }

  async has(hash: string): Promise<boolean> {
    const path = this.objectPath(hash);
    try {
      await stat(path);
      return true;
    } catch (error) {
      if (error instanceof Error && "code" in error && error.code === "ENOENT") {
        return false;
      }
      throw error;
    }
  }

  async verifyMissing(hashes: readonly string[]): Promise<string[]> {
    const missing: string[] = [];
    for (const hash of hashes) {
      if (!(await this.has(hash))) missing.push(hash);
    }
    return missing;
  }

  private objectPath(hash: string): string {
    if (!HASH_PATTERN.test(hash)) throw new ObjectStoreError(hash);
    return join(this.root, hash.slice(0, 2), hash);
  }

  private async syncDirectory(directory: string): Promise<void> {
    const handle = await open(directory, "r");
    try {
      await handle.sync();
    } finally {
      await handle.close();
    }
  }
}
