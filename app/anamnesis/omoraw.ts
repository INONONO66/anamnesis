import { createOmoRawParser } from "@anamnesis/backfill";
import { RpcClient } from "./client.ts";
import { ingestRawLane, jsonlOutside, textParser } from "./raw-lane.ts";

/** OMO transcript export lane; the shared pipeline in raw-lane.ts owns admission. */
export function ingestOmoRaw(root: string, checkpoint: string, client: RpcClient): Promise<void> {
  return ingestRawLane(root, checkpoint, client, {
    source: "omo-raw", format: "omo-raw-snapshot/1", lineage: true,
    selects: jsonlOutside("._"),
    parser: () => textParser(createOmoRawParser()),
  });
}
