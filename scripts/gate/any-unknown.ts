// Gate: no type escape hatches in shipped code. Boundary-typed `unknown` inputs are allowed; casts through any/unknown are not.
import { readFileSync } from "node:fs";
import { sourceFiles } from "./sources.ts";
const patterns: Array<[string, RegExp]> = [["as any", /\bas any\b/], ["as unknown as", /\bas unknown as\b/], [": any", /:\s*any\b/], ["<any>", /<any>/], ["@ts-ignore", /@ts-ignore/], ["@ts-expect-error", /@ts-expect-error/]];
const files = sourceFiles();
const hits: string[] = [];
for (const file of files) readFileSync(file, "utf8").split("\n").forEach((line, i) => { for (const [label, re] of patterns) if (re.test(line)) hits.push(`${file}:${i + 1} ${label}`); });
if (hits.length) { console.error(hits.join("\n")); console.error(`gate:types failed: ${hits.length} escape hatch(es)`); process.exit(1); }
console.log(`gate:types ok: 0 escape hatches in ${files.length} files`);
