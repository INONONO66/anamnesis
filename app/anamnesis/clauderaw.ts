import { classifyClaudeTranscript, createClaudeRawParser } from "@anamnesis/backfill";
import { RpcClient } from "./client.ts";
import { ingestRawLane } from "./raw-lane.ts";

/** Claude transcript export lane; the shared pipeline in raw-lane.ts owns admission. */
export function ingestClaudeRaw(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  return ingestRawLane(root, checkpoint, client, {
    source: "claude-raw", format: "claude-raw-snapshot/1", lineage: true,
    selects: local => local.endsWith(".jsonl") && classifyClaudeTranscript(local) !== undefined,
    parser: name => { const kind = classifyClaudeTranscript(name)!; return createClaudeRawParser({ path: name, ...(kind === "main" ? {} : { sidechain: kind }) }, true); },
  });
}
