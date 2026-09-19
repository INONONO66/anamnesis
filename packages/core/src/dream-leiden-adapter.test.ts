import { describe, expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { DREAM_ALGORITHM, DREAM_GDS_VERSION, DREAM_NETWORK, DREAM_GDS_IMAGE, DreamAdapterError, trustedDreamLeiden, type DreamLeidenInput } from "./dream-leiden-adapter.ts";

const input: DreamLeidenInput = { operation_id:"dream-1", export_bytes:"{}", export_digest:createHash("sha256").update("{}").digest("hex"), source_receipts:[], graph:{node_count:0,arc_count:0,byte_count:2,nodes:[],arcs:[]} };
test("refuses missing and unpinned adapters", () => {
  expect(() => trustedDreamLeiden(undefined)).toThrow(DreamAdapterError);
  expect(() => trustedDreamLeiden({ image_digest:DREAM_GDS_IMAGE, plugin_digest:"sha256:"+"0".repeat(64), algorithm:DREAM_ALGORITHM, gds_version:DREAM_GDS_VERSION, network:"bridge", adapter:{} })).toThrow("dream_adapter_untrusted");
});
describe("controlled fixture protocol", () => test("accepts only pinned deterministic result", async () => {
  const adapter = trustedDreamLeiden({ image_digest:DREAM_GDS_IMAGE, plugin_digest:"sha256:"+"0".repeat(64), algorithm:DREAM_ALGORITHM, gds_version:DREAM_GDS_VERSION, network:DREAM_NETWORK, adapter:{ execute: async (i: DreamLeidenInput) => { const assignments: { element_id:string; community_assignment:number }[] = []; const artifact_digest=createHash("sha256").update(JSON.stringify({export_digest:i.export_digest,assignments,image_digest:DREAM_GDS_IMAGE,gds_version:DREAM_GDS_VERSION,algorithm:DREAM_ALGORITHM,network:DREAM_NETWORK})).digest("hex"); return {export_digest:i.export_digest,artifact_digest,assignments,image_digest:DREAM_GDS_IMAGE,gds_version:DREAM_GDS_VERSION,algorithm:DREAM_ALGORITHM,network:DREAM_NETWORK,semantic_writes:false,deterministic:true,exit:"success"}; } } });
  expect((await adapter.execute(input)).network).toBe("none");
}));
