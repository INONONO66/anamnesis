import { createHash } from "node:crypto";
import { MemoryElement, type Origin, type TimePoint } from "@anamnesis/protocol";
import { EchoLineage, EpisodeLineageError, parseEpisodeLineage } from "@anamnesis/protocol";
import { type ValidatedSemanticClaim } from "@anamnesis/protocol";
import { canonicalExtractionBody } from "@anamnesis/protocol";
import { SemanticReviewPremises } from "@anamnesis/protocol";
import { carriesTime } from "./schema.ts";

export function semanticClaimTime(time: { value: string; precision: "second" | "minute" | "day" | "month" | "year" }) {
  const d = new Date(time.value);
  const precision = time.precision === "second" || time.precision === "minute" ? "instant" as const : time.precision;
  if (precision !== "instant") {
    d.setUTCHours(0, 0, 0, 0);
    if (precision !== "day") d.setUTCDate(1);
    if (precision === "year") d.setUTCMonth(0);
  }
  return { time_value: time.value, time_utc: d.getTime(), time_precision: precision, resolution: "explicit" as const, anchor_time_utc: null };
}

/** The TimePoint a validated claim is stored under; an inherited time keeps the
 * Episode's precision. The relation judge sees exactly this time. */
export function validatedFactTime(validated: ValidatedSemanticClaim, source: SemanticReviewPremises["source"]): TimePoint {
  const claim = validated.claim;
  const precision = claim.time.resolution === "inherited" ? source.time.time_precision : claim.time.time_precision;
  return { value: new Date(claim.time.time_utc).toISOString(), precision: precision === "instant" || precision === "inherited" ? "second" : precision };
}

export function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

/** Used when no snapshot cutoff is given, so every invalidator applies. */
export const END_OF_TIME = "9999-12-31T23:59:59.999Z";

export const CANONICAL_DIGEST = "rfc8785-v1";


export class StorageContractError extends Error {
  constructor(readonly code: "revision_conflict" | "stale_revision"
    | "unsupported_digest_format" | "invalid_canonical_json" | "unsupported_topology_format",
    readonly detail: string) {
    super(`${code}: ${detail}`);
  }
}

/** RFC 8785: UTF-16 key order, ECMAScript primitives, no lone surrogates.
 * Serialize members directly: JSON.stringify(object) reorders integer keys.
 * Values have already crossed the protocol's JSON-only boundary.
 */
export function canonicalJson(value: MemoryElement["properties"][string]): string {
  if (typeof value === "string" && /[\uD800-\uDFFF]/u.test(value)) {
    throw new StorageContractError("invalid_canonical_json", "lone surrogate");
  }
  if (value === null || typeof value !== "object") return JSON.stringify(value);
  if (Array.isArray(value)) return `[${value.map(canonicalJson).join(",")}]`;
  return `{${Object.entries(value).sort(([a], [b]) => a < b ? -1 : a > b ? 1 : 0)
    .map(([key, member]) => `${canonicalJson(key)}:${canonicalJson(member)}`).join(",")}}`;
}

interface DigestContext {
  payloadHash?: string | null;
  previousRevisionKey?: string | null;
  format?: string | number | null;
  episodeDigestVersion?: number | null;
  originRole?: string | null;
  lineageDigest?: string | null;
}

export function elementDigest(
  e: {
    schema: string;
    time?: TimePoint | undefined;
    content: string;
    properties?: MemoryElement["properties"] | undefined;
  },
  context: DigestContext = {},
): string {
  const body = {
      schema: e.schema,
      content: e.content,
      properties: Object.fromEntries(
        Object.entries(e.properties ?? {}).filter(
          ([key]) => key !== "payload_hash",
        ),
      ),
      time: carriesTime(e.schema) ? e.time ?? null : null,
      payload_hash: context.payloadHash ?? null,
      previous_revision_key: context.previousRevisionKey ?? null,
    };
  if (context.episodeDigestVersion != null) {
    if (context.episodeDigestVersion !== 2 || context.format !== "episode-rfc8785-v2")
      throw new EpisodeLineageError("unsupported_digest_version");
    return sha256(canonicalJson({ episode_digest_version: 2, ...body,
      origin_role: context.originRole ?? null, lineage_digest: context.lineageDigest ?? null }));
  }
  // Absent stored markers mean frozen insertion-ordered legacy bytes, never
  // an invitation to migrate or sort a previously admitted original.
  switch (context.format) {
    case null: return sha256(JSON.stringify(body));
    case undefined:
    case CANONICAL_DIGEST: return sha256(canonicalJson(body));
    default: throw new StorageContractError("unsupported_digest_format", String(context.format));
  }
}

export function verifyLineageRetry(input: unknown, role: string | null, lineage: EchoLineage): void {
  const metadata = parseEpisodeLineage(input);
  if (metadata.origin_role !== role || metadata.lineage_mode !== lineage.lineage_mode
    || canonicalExtractionBody(metadata.parent_recall_ids) !== canonicalExtractionBody(lineage.parent_recall_ids))
    throw new StorageContractError("revision_conflict", lineage.episode_id);
}

export function tupleHash(parts: readonly string[]): string {
  return sha256(JSON.stringify(parts));
}

export function originKey(o: Origin): string {
  return tupleHash([o.source, o.session, o.actor, o.record]);
}

export function sessionKey(o: Origin): string {
  return tupleHash([o.source, o.session]);
}

/** Originals-layer links are content-free keys; derived links bind content. */
export function linkIdemKey(
  l: {
    from: string;
    to: string;
    role: string;
    content: string;
  },
  originals: boolean,
): string {
  return sha256(
    JSON.stringify(
      originals
        ? [l.from, l.to, l.role]
        : [l.from, l.to, l.role, l.content],
    ),
  );
}
