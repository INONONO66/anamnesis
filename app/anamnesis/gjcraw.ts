import { createGjcRawParser } from "@anamnesis/backfill";
import { RpcClient } from "./client.ts";
import { ingestRawLane, textParser } from "./raw-lane.ts";

/** GJC session export lane; the shared pipeline in raw-lane.ts owns admission. */
export function ingestGjcRaw(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  return ingestRawLane(root, checkpoint, client, {
    source: "gjc-raw", format: "gjc-raw-snapshot/1", lineage: false,
    selects: local => local.endsWith(".jsonl") && local.includes("home/.gjc/agent/sessions/"),
    parser: () => textParser(createGjcRawParser()),
  });
}
