import { RoomAssignment, RpcEpisodeRebuildParams, type RpcEpisodeRebuildResult } from "@anamnesis/protocol";
import type { RpcClient } from "./client.ts";
import { jsonLines } from "./json-lines.ts";

/** Import in bounded wire batches. Replaying a file is an upsert, not a replacement. */
async function importRooms(client: RpcClient, path: string): Promise<void> {
  let batch: RoomAssignment[] = [], imported = 0, changed = 0;
  const flush = async () => {
    if (batch.length === 0) return;
    const result = await client.request("spaces.import", { assignments: batch });
    imported += result.imported; changed += result.changed; batch = [];
  };
  for await (const line of jsonLines(path)) {
    if (!line.trim()) continue;
    batch.push(RoomAssignment.parse(JSON.parse(line)));
    if (batch.length === 100) await flush();
  }
  await flush();
  console.log(JSON.stringify({ imported, changed }));
}

/** Every committed page prints its resume cursor; an interrupted page can be replayed. */
async function rebuild(client: RpcClient, command: "spaces" | "replies", args: readonly string[]): Promise<void> {
  const options = new Map<string, string>();
  for (let i = 0; i < args.length; i += 2) {
    const flag = args[i], value = args[i + 1];
    if (!flag || !["--source", "--room", "--after", "--through", "--limit"].includes(flag) || value === undefined || options.has(flag)) {
      throw new Error("rebuild expects --source SOURCE --room ROOM, --after N, --through N, --limit N");
    }
    options.set(flag, value);
  }
  if (options.has("--source") !== options.has("--room")) throw new Error("--source and --room must be supplied together");
  let params = RpcEpisodeRebuildParams.parse({
    ...(options.has("--source") ? { room: { source: options.get("--source"), room: options.get("--room") } } : {}),
    ...Object.fromEntries(["after", "through", "limit"].filter(key => options.has(`--${key}`)).map(key => [key, Number(options.get(`--${key}`))])),
  });
  for (;;) {
    const result: RpcEpisodeRebuildResult = await client.request(command === "spaces" ? "spaces.rebuild" : "replies.rebuild", params);
    console.log(JSON.stringify(result));
    if (result.done) return;
    params = { ...params, after: result.after, through: result.through };
  }
}

export async function spacesCommand(client: RpcClient, command: "spaces" | "replies", args: readonly string[]): Promise<void> {
  const [action, ...rest] = args;
  if (action === "rebuild") { await rebuild(client, command, rest); return; }
  if (command === "spaces" && action === "import" && rest.length === 1 && rest[0]) {
    await importRooms(client, rest[0]); return;
  }
  if (command === "spaces" && action === "list" && rest.length === 0) {
    let after: string | undefined;
    for (;;) {
      const result = await client.request("spaces.list", { limit: 100, ...(after ? { after } : {}) });
      for (const space of result.spaces) console.log(JSON.stringify(space));
      if (result.next === null) return;
      after = result.next;
    }
  }
  throw new Error("usage: spaces import <rooms.jsonl> | spaces list | spaces rebuild | replies rebuild [--source SOURCE --room ROOM] [--after N --through N] [--limit N]");
}
