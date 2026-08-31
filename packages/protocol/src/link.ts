import { z } from "zod";
import type { Celestial } from "./element.ts";

/** Roles stay closed because the engine consumes each role structurally. */
export const LinkRole = z.enum([
  "MENTIONS",
  "RELATES_TO",
  "NEXT_EPISODE",
  "HAS_MEMBER",
  "DERIVED_FROM",
  "INVALIDATES",
  "CONTRASTS",
]);
export type LinkRole = z.infer<typeof LinkRole>;

/** Invalid label pairs are rejected before they can weaken graph semantics. */
export const LINK_LATTICE: Record<
  LinkRole,
  { from: readonly Celestial[]; to: readonly Celestial[] }
> = {
  NEXT_EPISODE: { from: ["Episode"], to: ["Episode"] },
  MENTIONS: { from: ["Episode", "Fact"], to: ["Entity"] },
  RELATES_TO: { from: ["Fact", "Entity"], to: ["Fact", "Entity"] },
  HAS_MEMBER: { from: ["Community"], to: ["Entity", "Fact"] },
  DERIVED_FROM: { from: ["Fact"], to: ["Episode", "Fact"] },
  INVALIDATES: { from: ["Fact", "Episode"], to: ["Fact", "Episode"] },
  CONTRASTS: { from: ["Fact"], to: ["Fact"] },
};

export const MemoryLink = z
  .object({
    id: z.uuid(),
    from: z.uuid(),
    to: z.uuid(),
    role: LinkRole,
    content: z.string().min(1),
    weight: z.number().positive().default(1),
  })
  .strict()
  .refine((link) => link.from !== link.to, {
    message: "self-link is not allowed",
  });
export type MemoryLink = z.infer<typeof MemoryLink>;
export type MemoryLinkInput = z.input<typeof MemoryLink>;
