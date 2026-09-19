import { z } from "zod";

export const OriginRole = z.enum(["user", "assistant", "tool", "document", "operator"]);
const ids = (max: number) => z.array(z.uuidv7()).max(max).refine(v => new Set(v).size === v.length, "IDs must be distinct");
/** Required only on the explicit authenticated path, after stored-version selection. */
export const EpisodeLineageInput = z.strictObject({
  origin_role: OriginRole,
  lineage_mode: z.enum(["direct", "receipts"]),
  parent_recall_ids: ids(4),
}).superRefine((v, ctx) => {
  if ((v.lineage_mode === "direct") !== (v.parent_recall_ids.length === 0))
    ctx.addIssue({ code: "custom", message: "direct requires no parents; receipts requires parents" });
});
export type EpisodeLineageInput = z.infer<typeof EpisodeLineageInput>;
const sortedIds = (max: number) => ids(max).refine(v => v.every((id, i) => i === 0 || v[i - 1]! < id), "IDs must be sorted");
export const EchoLineage = z.strictObject({
  episode_id: z.uuidv7(), lineage_mode: z.enum(["direct", "receipts"]),
  parent_recall_ids: sortedIds(4), context_digests: z.array(z.string().regex(/^[0-9a-f]{64}$/)).max(4),
  root_episode_ids: sortedIds(16), echo_depth: z.number().int().min(0).max(8), complete: z.boolean(),
}).superRefine((v, ctx) => {
  if (v.parent_recall_ids.length !== v.context_digests.length
    || (v.lineage_mode === "direct" && (v.parent_recall_ids.length !== 0 || v.echo_depth !== 0 || !v.complete
      || v.root_episode_ids.length !== 1 || v.root_episode_ids[0] !== v.episode_id))
    || (v.lineage_mode === "receipts" && (v.parent_recall_ids.length === 0 || v.echo_depth === 0))
    || (v.complete && v.root_episode_ids.length === 0))
    ctx.addIssue({ code: "custom", message: "invalid retained lineage" });
});
export type EchoLineage = z.infer<typeof EchoLineage>;
/** Snapshots are server materialized. Empty roots mean unknown, never direct. */
export const RecallLineageSelection = z.array(z.strictObject({
  element_id: z.uuidv7(), root_episode_ids: sortedIds(16),
  echo_depth: z.number().int().min(0).max(8), complete: z.boolean(),
})).max(64);
export class EpisodeLineageError extends Error {
  constructor(readonly code: "invalid_params" | "lineage_unavailable" | "lineage_binding_mismatch" | "lineage_mismatch" | "unsupported_digest_version") { super(code); }
}
export function parseEpisodeLineage(input: unknown): EpisodeLineageInput {
  const result = EpisodeLineageInput.safeParse(input);
  if (!result.success) throw new EpisodeLineageError("invalid_params");
  return { ...result.data, parent_recall_ids: [...result.data.parent_recall_ids].sort() };
}
