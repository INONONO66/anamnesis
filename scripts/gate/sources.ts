// The shipped TypeScript the gates measure: every non-test, non-fixture .ts under the runtime and package roots, sorted.
import { readdirSync, statSync } from "node:fs";
import { join } from "node:path";

const roots = ["app/anamnesis", "packages/protocol/src", "packages/core/src", "packages/backfill/src"];

export function sourceFiles(): string[] {
  const files: string[] = [];
  const walk = (dir: string): void => {
    for (const name of readdirSync(dir)) {
      const path = join(dir, name);
      if (statSync(path).isDirectory()) walk(path);
      else if (path.endsWith(".ts") && !/\.(test|fixture)\.ts$/.test(path)) files.push(path);
    }
  };
  roots.forEach(walk);
  return files.sort();
}
