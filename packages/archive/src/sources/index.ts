import { agentlogAdapter } from "./agentlog.ts";
import { claudeAdapter } from "./claude.ts";
import { codexAdapter } from "./codex.ts";
import { gjcAdapter } from "./gjc.ts";
import { miscAdapter } from "./misc.ts";
import { notionAdapter } from "./notion.ts";
import { omoAdapter } from "./omo.ts";
import { piAdapter } from "./pi.ts";
import { slackAdapter } from "./slack.ts";
import type { SourceAdapter } from "./types.ts";

export const adapters: Record<string, SourceAdapter> = {
  omo: omoAdapter,
  pi: piAdapter,
  gjc: gjcAdapter,
  misc: miscAdapter,
  claude: claudeAdapter,
  codex: codexAdapter,
  slack: slackAdapter,
  notion: notionAdapter,
  agentlog: agentlogAdapter,
};
