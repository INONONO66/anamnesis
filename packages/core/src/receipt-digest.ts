/** episode-selection-v1 body-digest projection. The issuer supplies parsed
 * request/serving values; readers supply the persisted receipt. Authority,
 * selection and transport metadata have their own contracts, not this digest.
 * Preserve serving verbatim, including clocks, IDs, rankings and Unicode. */
export function canonicalReceiptJson(value: unknown): string {
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalReceiptJson).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
    .map(([key, member]) => `${JSON.stringify(key)}:${canonicalReceiptJson(member)}`).join(",")}}`;
}

export function receiptBodyDigestInput<T extends {
  recall_id: string; primary_ids: readonly string[]; receipt_ttl_ms: number; serving?: unknown;
}>(value: T) {
  return { recall_id: value.recall_id, primary_ids: value.primary_ids, receipt_ttl_ms: value.receipt_ttl_ms,
    ...(value.serving === undefined ? {} : { serving: value.serving }) };
}
