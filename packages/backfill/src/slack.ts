import { readdir, readFile } from "node:fs/promises";
import { basename, join } from "node:path";
import type { RememberInput } from "@anamnesis/core";
import { maskSecrets } from "./secrets.ts";

/** Only the fields the originals contract consumes are modelled. */
interface SlackMessage {
  ts: string;
  text: string;
  user: string;
  subtype?: string;
  thread_ts?: string;
  edited?: { ts: string };
}

function text(value: unknown, fallback: string): string {
  return typeof value === "string" && value !== "" ? value : fallback;
}

function optionalText(value: unknown): string | undefined {
  return typeof value === "string" && value !== "" ? value : undefined;
}

function parseMessage(line: string): SlackMessage {
  const raw: Record<string, unknown> = JSON.parse(line);
  const ts = optionalText(raw["ts"]);
  if (ts === undefined) throw new Error(`slack message without ts: ${line}`);
  const edited = raw["edited"];
  const editedTs =
    typeof edited === "object" && edited !== null
      ? optionalText((edited as Record<string, unknown>)["ts"])
      : undefined;
  const subtype = optionalText(raw["subtype"]);
  const threadTs = optionalText(raw["thread_ts"]);
  return {
    ts,
    text: text(raw["text"], ""),
    user: text(raw["user"], "unknown"),
    ...(subtype === undefined ? {} : { subtype }),
    ...(threadTs === undefined ? {} : { thread_ts: threadTs }),
    ...(editedTs === undefined ? {} : { edited: { ts: editedTs } }),
  };
}

/** Membership and housekeeping events carry no recallable content. */
const SLOP_SUBTYPES = new Set([
  "channel_join",
  "channel_leave",
  "group_join",
  "group_leave",
  "channel_topic",
  "channel_purpose",
  "channel_name",
  "mpdm_move",
  "huddle_thread",
  "bot_message",
]);

/** Localized join notices arrive without a subtype in these exports. */
const JOIN_NOTICE =
  /^<@[A-Z0-9]+>(?:さんがチャンネルに参加しました|님이 채널에 참여했습니다| has joined the channel)$/;

export interface SlackEpisode {
  input: RememberInput;
  redactions: number;
}

function isSlop(message: SlackMessage): boolean {
  if (message.subtype !== undefined && SLOP_SUBTYPES.has(message.subtype)) {
    return true;
  }
  const text = message.text.trim();
  return text === "" || JOIN_NOTICE.test(text);
}

function toEpisode(
  message: SlackMessage,
  channelId: string,
  channelName: string,
): SlackEpisode {
  const { text, redactions } = maskSecrets(message.text);
  const seconds = Number.parseFloat(message.ts);
  const properties: Record<string, string> = {
    channel_name: channelName,
    slack_ts: message.ts,
  };
  if (message.thread_ts !== undefined && message.thread_ts !== message.ts) {
    properties["thread_parent_ts"] = message.thread_ts;
  }
  return {
    redactions,
    input: {
      schema: "anamnesis.original-message/1",
      content: text,
      origin: {
        source: "slack",
        session: channelId,
        actor: message.user,
        record: message.ts,
      },
      source_revision: message.edited?.ts ?? message.ts,
      time: {
        value: new Date(seconds * 1000).toISOString(),
        precision: "second",
      },
      properties,
    },
  };
}

async function readMessages(path: string): Promise<SlackMessage[]> {
  const raw = await readFile(path, "utf8");
  return raw
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map(parseMessage);
}

/** Channel id prefixes every thread export: `<channel>-<parent ts>.jsonl`. */
function channelOf(fileName: string): string {
  return basename(fileName).split("-")[0] ?? "";
}

async function jsonlFiles(directory: string): Promise<string[]> {
  const entries = await readdir(directory).catch(() => []);
  return entries.filter((entry) => entry.endsWith(".jsonl"));
}

/**
 * Channel and thread exports are merged per channel and ordered by event time
 * so the session topology the store rebuilds matches conversation order.
 */
export async function collectSlack(root: string): Promise<SlackEpisode[]> {
  const index = await readFile(join(root, "index.jsonl"), "utf8");
  const names = new Map<string, string>();
  for (const line of index.split("\n").filter((l) => l.trim() !== "")) {
    const channel: Record<string, unknown> = JSON.parse(line);
    const id = optionalText(channel["id"]);
    const name = optionalText(channel["name"]);
    if (id !== undefined && name !== undefined) names.set(id, name);
  }

  const byChannel = new Map<string, SlackMessage[]>();
  const add = (channelId: string, messages: SlackMessage[]): void => {
    const bucket = byChannel.get(channelId) ?? [];
    bucket.push(...messages);
    byChannel.set(channelId, bucket);
  };

  const channelsDir = join(root, "channels");
  for (const file of await jsonlFiles(channelsDir)) {
    add(basename(file, ".jsonl"), await readMessages(join(channelsDir, file)));
  }
  const threadsDir = join(root, "threads");
  for (const file of await jsonlFiles(threadsDir)) {
    add(channelOf(file), await readMessages(join(threadsDir, file)));
  }

  const episodes: SlackEpisode[] = [];
  for (const [channelId, messages] of [...byChannel].sort(([a], [b]) =>
    a.localeCompare(b),
  )) {
    const kept = messages.filter((message) => !isSlop(message));
    kept.sort((a, b) => Number.parseFloat(a.ts) - Number.parseFloat(b.ts));
    for (const message of kept) {
      episodes.push(toEpisode(message, channelId, names.get(channelId) ?? channelId));
    }
  }
  return episodes;
}
