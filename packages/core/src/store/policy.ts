import { RpcPolicySetParams } from "@anamnesis/protocol";
import { z } from "zod";
import { canonicalJson } from "./digest.ts";
import { receiptTime, ReceiptError } from "./receipts.ts";

export interface InstallationContext { readonly principal: "installation"; readonly commit_mode: "auto" | "receipt"; readonly client_binding?: string }
export const PolicyEvent = RpcPolicySetParams.extend({
  action: z.enum(["deny", "revoke"]), principal: z.literal("installation"),
  revision: receiptTime.positive(), created_at: receiptTime,
});
export type PolicyEvent = z.infer<typeof PolicyEvent>;
export function policySelector(selector: RpcPolicySetParams["selector"]): Record<string, string> {
  return { ...(selector.episode_id === undefined ? {} : { episode_id: selector.episode_id }),
    ...(selector.source === undefined ? {} : { source: selector.source }) };
}
export function policyBody(value: RpcPolicySetParams | PolicyEvent): string {
  return canonicalJson({ ...value, selector: policySelector(value.selector) });
}
export type PolicyState = { structure_revision: number | null; policy_revision: number; denies: Map<string, PolicyEvent>; revoked: Set<string> };
export function requireInstallation(context: InstallationContext): void {
  if (context?.principal !== "installation") throw new ReceiptError("unauthenticated");
}
