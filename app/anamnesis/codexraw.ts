import { createCodexRawParser } from "@anamnesis/backfill";
import { RpcClient } from "./client.ts";
import { ingestRawLane, jsonlOutside, textParser } from "./raw-lane.ts";

/** Codex rollout export lane; the shared pipeline in raw-lane.ts owns admission. */
export function ingestCodexRaw(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  return ingestRawLane(root, checkpoint, client, {
    source: "codex-raw", format: "codex-raw-snapshot/1", lineage: true,
    selects: jsonlOutside("sessions-"),
    parser: name => textParser(createCodexRawParser(name.split("/").pop()?.replace(/\.jsonl$/, "") ?? name, true)),
  });
}
