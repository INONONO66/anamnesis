import { createHash } from "node:crypto";
import { z } from "zod";

export const DREAM_GDS_IMAGE = "sha256:2131200000000000000000000000000000000000000000000000000000000000" as const;
export const DREAM_GDS_VERSION = "2.13.12" as const;
export const DREAM_ALGORITHM = "leiden" as const;
export const DREAM_NETWORK = "none" as const;
export const DREAM_LIMITS = { export_bytes: 4_000_000, nodes: 100_000, arcs: 500_000 } as const;

const hash = z.string().regex(/^[a-f0-9]{64}$/);
const image = z.templateLiteral([z.literal("sha256:"), z.string().regex(/^[a-f0-9]{64}$/)]);
export const DreamLeidenInput = z.strictObject({
  operation_id: z.string().min(1).max(256), export_bytes: z.string().max(DREAM_LIMITS.export_bytes),
  export_digest: hash, source_receipts: z.array(z.strictObject({ id:z.string(), revision:hash, body_digest:hash, ingest_seq:z.number().int().positive(), allowed:z.literal(true) })).max(256),
  graph: z.strictObject({ node_count:z.number().int().nonnegative().max(DREAM_LIMITS.nodes), arc_count:z.number().int().nonnegative().max(DREAM_LIMITS.arcs), byte_count:z.number().int().nonnegative().max(DREAM_LIMITS.export_bytes), nodes:z.array(z.string().min(1).max(256)).max(DREAM_LIMITS.nodes), arcs:z.array(z.strictObject({from:z.string().min(1).max(256),to:z.string().min(1).max(256),weight:z.number().finite().positive()})).max(DREAM_LIMITS.arcs) }),
});
export type DreamLeidenInput = z.infer<typeof DreamLeidenInput>;
export const DreamLeidenOutput = z.strictObject({ export_digest:hash, artifact_digest:hash, assignments:z.array(z.strictObject({element_id:z.string(),community_assignment:z.number().int().nonnegative()})).max(DREAM_LIMITS.nodes), image_digest:image, gds_version:z.literal(DREAM_GDS_VERSION), algorithm:z.literal(DREAM_ALGORITHM), network:z.literal(DREAM_NETWORK), semantic_writes:z.literal(false), deterministic:z.literal(true), exit:z.enum(["success","unknown"]) });
export type DreamLeidenOutput = z.infer<typeof DreamLeidenOutput>;
export class DreamAdapterError extends Error { constructor(readonly code:"dream_adapter_unavailable"|"dream_adapter_untrusted"|"dream_result_unverified", message=code){super(message);this.name="DreamAdapterError";} }
export type DreamLeidenAdapter = { execute(input: DreamLeidenInput): Promise<DreamLeidenOutput> };
export type TrustedDreamLeidenConfig = { image_digest: typeof DREAM_GDS_IMAGE; plugin_digest: `sha256:${string}`; algorithm: typeof DREAM_ALGORITHM; gds_version: typeof DREAM_GDS_VERSION; network: typeof DREAM_NETWORK; adapter: DreamLeidenAdapter };
export function trustedDreamLeiden(config: unknown): DreamLeidenAdapter {
  if (!config || typeof config !== "object") throw new DreamAdapterError("dream_adapter_unavailable");
  const c=config as Partial<TrustedDreamLeidenConfig>;
  if (c.image_digest!==DREAM_GDS_IMAGE || c.algorithm!==DREAM_ALGORITHM || c.gds_version!==DREAM_GDS_VERSION || c.network!==DREAM_NETWORK || typeof c.plugin_digest!=="string" || !/^sha256:[a-f0-9]{64}$/.test(c.plugin_digest) || !c.adapter) throw new DreamAdapterError("dream_adapter_untrusted");
  return { execute: async input => { const parsed=DreamLeidenInput.parse(input); const result=DreamLeidenOutput.parse(await c.adapter!.execute(parsed)); if(result.export_digest!==parsed.export_digest || result.image_digest!==c.image_digest) throw new DreamAdapterError("dream_result_unverified"); const expected=createHash("sha256").update(JSON.stringify({export_digest:result.export_digest,assignments:result.assignments,image_digest:result.image_digest,gds_version:result.gds_version,algorithm:result.algorithm,network:result.network})).digest("hex"); if(result.artifact_digest!==expected) throw new DreamAdapterError("dream_result_unverified"); return result; } };
}
