import { afterEach, describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { mkdtemp, readdir, rm, stat } from "node:fs/promises";
import { join } from "node:path";
import { tmpdir } from "node:os";
import { ObjectStore, ObjectStoreError } from "./objects.ts";

const directories: string[] = [];

afterEach(async () => {
  await Promise.all(directories.splice(0).map((directory) => rm(directory, { recursive: true })));
});

async function temporaryStore(): Promise<{ root: string; store: ObjectStore }> {
  const root = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
  directories.push(root);
  return { root, store: new ObjectStore(root) };
}

function digest(bytes: Uint8Array): string {
  return createHash("sha256").update(bytes).digest("hex");
}

describe("ObjectStore", () => {
  test("round-trips bytes and reports metadata", async () => {
    const { store } = await temporaryStore();
    const bytes = Uint8Array.from([0, 1, 127, 255]);

    const result = await store.put(bytes, "application/octet-stream");

    expect(result).toEqual({
      hash: digest(bytes),
      size: bytes.byteLength,
      mediaType: "application/octet-stream",
    });
    expect(await store.get(result.hash)).toEqual(bytes);
    expect(await store.has(result.hash)).toBe(true);
  });

  test("stores the object under its sha256 path", async () => {
    const { root, store } = await temporaryStore();
    const bytes = new TextEncoder().encode("content addressed");
    const hash = digest(bytes);

    await store.put(bytes, "text/plain");

    expect(await stat(join(root, hash.slice(0, 2), hash))).toMatchObject({ size: bytes.length });
  });

  test("does not rewrite an existing object on a repeated put", async () => {
    const { root, store } = await temporaryStore();
    const bytes = new TextEncoder().encode("write once");
    const hash = digest(bytes);

    const first = await store.put(bytes, "text/plain");
    const before = await stat(join(root, hash.slice(0, 2), hash));
    const second = await store.put(bytes, "text/plain");
    const after = await stat(join(root, hash.slice(0, 2), hash));

    expect(second).toEqual(first);
    expect(after.ino).toBe(before.ino);
    expect((await readdir(join(root, hash.slice(0, 2)))).filter((name) => name === hash)).toHaveLength(1);
  });

  test("rejects path traversal and other invalid hashes with a typed error", async () => {
    const { store } = await temporaryStore();

    await expect(store.get("../../x")).rejects.toBeInstanceOf(ObjectStoreError);
    await expect(store.has("../../x")).rejects.toBeInstanceOf(ObjectStoreError);
    await expect(store.verifyMissing(["../../x"])).rejects.toBeInstanceOf(ObjectStoreError);
  });

  test("reports hashes whose object files are missing", async () => {
    const { root, store } = await temporaryStore();
    const present = new TextEncoder().encode("present");
    const missing = digest(new TextEncoder().encode("missing"));
    const presentHash = (await store.put(present, "text/plain")).hash;

    await rm(join(root, presentHash.slice(0, 2), presentHash));

    expect(await store.verifyMissing([presentHash, missing])).toEqual([presentHash, missing]);
    expect(await store.has(missing)).toBe(false);
  });
});
