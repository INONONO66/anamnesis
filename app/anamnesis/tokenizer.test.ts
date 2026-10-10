import assert from "node:assert/strict";
import { createHash } from "node:crypto";
import { mkdtemp, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { test } from "node:test";
import { loadTokenizers } from "./tokenizer.ts";

test("tokenizer digest and encoder use assets in name order regardless of configuration order", async () => {
  const root = await mkdtemp(join(tmpdir(), "ana-tokenizer-order-"));
  try {
    const code = Buffer.from("module.exports.createEncoder = assets => text => text ? assets.alpha.length + assets.zeta.length : 0;");
    const alpha = Buffer.from("a"), zeta = Buffer.from("zz");
    const hash = (bytes: Buffer) => createHash("sha256").update(bytes).digest("hex");
    const path = join(root, "encoder.cjs"), alphaPath = join(root, "alpha"), zetaPath = join(root, "zeta");
    await writeFile(path, code);
    await writeFile(alphaPath, alpha);
    await writeFile(zetaPath, zeta);
    const manifest = [["alpha", hash(alpha)], ["zeta", hash(zeta)]];
    const digest = hash(Buffer.from(JSON.stringify(["anamnesis-tokenizer-v1", hash(code), manifest])));
    const id = `ordered@sha256:${digest}`;
    const config = { id, path, assets: [{ name: "zeta", path: zetaPath }, { name: "alpha", path: alphaPath }] };

    const encoder = loadTokenizers(JSON.stringify(config)).get(id);

    assert.equal(encoder?.("text"), 3);
    assert.equal(encoder?.(""), 0);
  } finally { await rm(root, { recursive: true, force: true }); }
});
