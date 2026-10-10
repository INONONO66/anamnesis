import { v7 as uuidv7 } from "uuid";
import type { InstallationContext } from "./store/policy.ts";
import type { Engine } from "./engine.ts";

const RECALL_CONTEXT: InstallationContext = {
  principal: "installation",
  commit_mode: "receipt",
  client_binding: uuidv7(),
};

export async function recallEpisodes(
  engine: Engine,
  query: string,
  options: { limit?: number; at?: string } = {},
) {
  const response = await engine.recallHybrid({
    query,
    limit: options.limit ?? 10,
    ...(options.at ? { T: Date.parse(options.at) } : {}),
  }, RECALL_CONTEXT);
  return response.results
    .filter(item => item.kind === "Episode")
    .map(item => ({ element: { id: item.id, content: item.content }, score: item.score }));
}
