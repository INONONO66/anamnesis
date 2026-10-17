import { parseArgs } from "node:util";
import { z } from "zod";
import { RpcClient } from "../../../app/anamnesis/client.ts";
import { uploadPayload } from "../../../app/anamnesis/source.ts";
import { admit, type Rememberer } from "./admit.ts";
import { collect } from "./collect.ts";
import { Manifest } from "./manifest.ts";
import { adapters } from "./sources/index.ts";

const USAGE = [
  "usage: bun packages/archive/src/cli.ts collect <source> <root> --archive <dir>",
  "       bun packages/archive/src/cli.ts admit --archive <dir> --socket <uds path> --token <token> [--source <kind>] [--since <iso-datetime>]",
].join("\n");

function usage(): never {
  console.error(USAGE);
  process.exit(2);
}

function sourcesIn(archiveDir: string): string[] {
  const manifest = Manifest.open(archiveDir);
  try {
    return manifest.countsBySource().map((row) => row.source);
  } finally {
    manifest.close();
  }
}

async function runCollect(argv: string[]): Promise<void> {
  const [source, root, flag, archiveDir] = argv;
  if (source === undefined || root === undefined || flag !== "--archive" || archiveDir === undefined || argv.length !== 4) usage();
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
}

async function runAdmit(argv: string[]): Promise<void> {
  let values: { archive?: string; socket?: string; token?: string; source?: string; since?: string };
  try {
    ({ values } = parseArgs({ args: argv, strict: true, allowPositionals: false, options: {
      archive: { type: "string" }, socket: { type: "string" }, token: { type: "string" }, source: { type: "string" }, since: { type: "string" },
    } }));
  } catch {
    usage();
  }
  const { archive, socket, token, source, since } = values;
  if (archive === undefined || socket === undefined || token === undefined) usage();
  if (since !== undefined && !z.iso.datetime().safeParse(since).success) {
    console.error(`invalid --since: ${since}`);
    process.exit(2);
  }
  const present = sourcesIn(archive);
  if (source !== undefined && !present.includes(source)) {
    console.error(`unknown source: ${source}; archived kinds: ${present.join(", ")}`);
    process.exit(2);
  }
  let signal: number | undefined;
  process.on("SIGINT", () => { signal ??= 130; });
  process.on("SIGTERM", () => { signal ??= 143; });
  const client = await RpcClient.connect(socket, token);
  try {
    const rememberer: Rememberer = {
      async remember(params, payload) {
        if (payload !== undefined) await uploadPayload(client, { params, payload });
        return client.request("remember", params);
      },
    };
    for (const kind of source === undefined ? present : [source]) {
      const report = await admit({ archiveDir: archive, source: kind, client: rememberer, stopped: () => signal !== undefined, ...(since === undefined ? {} : { since }) });
      console.log(JSON.stringify(report));
      if (signal !== undefined) break;
    }
  } finally {
    await client.close();
  }
  if (signal !== undefined) process.exit(signal);
}

const [command, ...rest] = process.argv.slice(2);
if (command === "collect") await runCollect(rest);
else if (command === "admit") await runAdmit(rest);
else usage();
