/**
 * zod 계약 → JSON Schema export.
 * TS 밖의 소비자(도구·문서·향후 다른 언어 구현)를 위한 언어 중립 계약.
 */
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { z } from "zod";
import { MemoryElement, MemoryLink } from "../src/index.ts";

const outDir = join(import.meta.dir, "..", "schemas");
await mkdir(outDir, { recursive: true });

const targets = {
  "memory-element": MemoryElement,
  "memory-link": MemoryLink,
} as const;

for (const [name, schema] of Object.entries(targets)) {
  const json = z.toJSONSchema(schema, { target: "draft-2020-12" });
  await writeFile(
    join(outDir, `${name}.schema.json`),
    JSON.stringify(json, null, 2) + "\n",
  );
  console.log(`exported schemas/${name}.schema.json`);
}
