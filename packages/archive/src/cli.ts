import { collect } from "./collect.ts";
import { Manifest } from "./manifest.ts";
import { adapters } from "./sources/index.ts";

const [command, source, root, flag, archiveDir] = process.argv.slice(2);
if (command !== "collect" || source === undefined || root === undefined || flag !== "--archive" || archiveDir === undefined || process.argv.length !== 7) {
  console.error("usage: bun packages/archive/src/cli.ts collect <source> <root> --archive <dir>");
  process.exit(2);
}

const adapter = adapters[source];
if (adapter === undefined) {
  console.error(`unknown source: ${source}; known kinds: ${Object.keys(adapters).sort().join(", ")}`);
  process.exit(2);
}

await collect({ adapter, root, archiveDir });
const manifest = Manifest.open(archiveDir);
try {
  for (const row of manifest.countsBySource()) {
    console.log(`${row.source}\t${row.sessions}\t${row.records}`);
  }
} finally {
  manifest.close();
}
