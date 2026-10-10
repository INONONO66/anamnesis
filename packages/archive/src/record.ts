import { createHash } from "node:crypto";
import { z } from "zod";

export const ArchiveRole = z.enum(["user", "assistant", "system", "developer", "tool", "document", "operator"]);
export const ArchiveCanonicalKind = z.enum(["message", "compaction", "summary", "system-event", "other"]);

export const ArchiveRecord = z.object({
  origin: z.object({
    source: z.string().min(1),
    session: z.string().min(1),
    actor: z.string().min(1),
    record: z.string().min(1),
  }),
  seq: z.number().int().nonnegative(),
  role: ArchiveRole,
  occurred_at: z.iso.datetime({ offset: true }),
  time_precision: z.enum(["second", "minute", "day"]),
  text: z.string(),
  reasoning: z.string().optional(),
  kind: z.string().min(1),
  canonical_kind: ArchiveCanonicalKind,
  revision: z.string().regex(/^[0-9a-f]{64}$/),
  file: z.object({
    sha256: z.string().regex(/^[0-9a-f]{64}$/),
    path: z.string().min(1),
    line: z.number().int().nonnegative(),
  }),
});

export type ArchiveRecord = z.infer<typeof ArchiveRecord>;

export function revisionOf(occurredAt: string, text: string): string {
  return createHash("sha256").update(`${occurredAt}\n${text}`, "utf8").digest("hex");
}
