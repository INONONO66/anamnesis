import { Engine, EpisodeJournal, journaledRemember } from "@anamnesis/core";
import type { RememberInput } from "@anamnesis/core";
import { collectAgentLog } from "./agentlog.ts";
import { collectGjcRaw } from "./gjcraw.ts";
import { collectNotion } from "./notion.ts";
import { collectSlack } from "./slack.ts";

interface Collected {
  input: RememberInput;
  redactions: number;
}

const COLLECTORS: Record<string, (root: string) => Promise<Collected[]>> = {
  slack: collectSlack,
  notion: collectNotion,
  agentlog: collectAgentLog,
  gjcraw: collectGjcRaw,
};

interface Receipt {
  source: string;
  episodes: number;
  created: number;
  duplicates: number;
  invalidated: number;
  /** A stored revision whose content changed: an adapter reused one token. */
  diverged: number;
  redactions: number;
}

async function main(): Promise<void> {
  const [source, root, journalDirectory] = process.argv.slice(2);
  const collect = source === undefined ? undefined : COLLECTORS[source];
  if (source === undefined || collect === undefined || root === undefined) {
    throw new Error(
      `usage: bun backfill/run.ts <${Object.keys(COLLECTORS).join("|")}> <dataset-root> [journal-dir]`,
    );
  }

  const collected = await collect(root);
  const engine = new Engine();
  await engine.init();
  const journal =
    journalDirectory === undefined
      ? undefined
      : new EpisodeJournal(journalDirectory);

  const receipt: Receipt = {
    source,
    episodes: collected.length,
    created: 0,
    duplicates: 0,
    invalidated: 0,
    diverged: 0,
    redactions: 0,
  };
  try {
    for (const item of collected) {
      const result =
        journal === undefined
          ? await engine.remember(item.input)
          : await journaledRemember(journal, engine, item.input);
      if (result.created) receipt.created += 1;
      else receipt.duplicates += 1;
      if (result.invalidated !== undefined) receipt.invalidated += 1;
      if (result.diverged === true) receipt.diverged += 1;
      receipt.redactions += item.redactions;
    }
  } finally {
    await engine.close();
  }
  process.stdout.write(`${JSON.stringify(receipt)}\n`);
}

await main();
