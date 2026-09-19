import { createHash } from "node:crypto";
import { z } from "zod";

const uuidv7 = z.uuidv7();
const version = z.string().min(1).max(256);

/** The only generation identity accepted by serving/profile contracts. */
export const GenerationIdentity = z.strictObject({
  generation_id: uuidv7,
  generation_version: version,
});
export type GenerationIdentity = z.infer<typeof GenerationIdentity>;

const ProfileInput = z.union([
  z.strictObject({ generation_id: uuidv7, profile_version: version, coverage_generation_id: uuidv7 }),
  // Numeric ordered-regression output is an audit artifact, not a generation identity.
  z.strictObject({ generation: z.number().int().nonnegative(), profile_version: version, coverage_generation_id: uuidv7 }),
]);
export type GenerationProfileInput = z.infer<typeof ProfileInput>;

export const GenerationIdentityReceipt = z.strictObject({
  generation_id: uuidv7,
  generation_version: version,
  profile_version: version,
  coverage_generation_id: uuidv7,
  digest: z.string().regex(/^[0-9a-f]{64}$/),
});
export type GenerationIdentityReceipt = z.infer<typeof GenerationIdentityReceipt>;

export type BoundGenerationProfile = { identity: GenerationIdentity; receipt: GenerationIdentityReceipt };

/**
 * Binds an ordered profile to the UUID generation it actually read. There is
 * deliberately no numeric-to-UUID conversion: accepting one would permit ABA
 * reuse and make a profile appear valid for a different generation.
 */
export function bindGenerationProfile(identityInput: unknown, profileInput: unknown): BoundGenerationProfile {
  const identity = GenerationIdentity.parse(identityInput);
  const profile = ProfileInput.parse(profileInput);
  if (!("generation_id" in profile)) throw new Error("generation_identity_refused");
  if (profile.generation_id !== identity.generation_id) throw new Error("generation_identity_mismatch");
  if (profile.coverage_generation_id !== identity.generation_id) throw new Error("coverage_identity_mismatch");
  const receiptInput = {
    generation_id: identity.generation_id,
    generation_version: identity.generation_version,
    profile_version: profile.profile_version,
    coverage_generation_id: profile.coverage_generation_id,
  };
  const digest = createHash("sha256").update(JSON.stringify(receiptInput)).digest("hex");
  return { identity, receipt: GenerationIdentityReceipt.parse({ ...receiptInput, digest }) };
}
