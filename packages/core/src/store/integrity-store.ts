import { MemoryElement } from "@anamnesis/protocol";
import { EpisodeLineageError } from "@anamnesis/protocol";
import { historicalEligibility, type EligibilityReason } from "../legacy-format.ts";
import { z } from "zod";
import { type TopologyRow, TOPOLOGY_QUERY, topologyExpectations } from "./conducting.ts";
import { sha256, CANONICAL_DIGEST, StorageContractError, elementDigest, tupleHash } from "./digest.ts";
import { type ElementProperties, type ElementNode, nodeProps, StoredHash, decodeHistoricalElement, toElement } from "./records.ts";
import type { StoreCore } from "./core.ts";
import type { ReceiptStore } from "./receipt-store.ts";
import type { ElementStore } from "./element-store.ts";

export interface IntegrityIssue {
  elementId: string;
  kind: "digest-mismatch" | "missing-payload" | "payload-hash-mismatch"
    | "unsupported-digest-format" | "topology-mismatch" | "unsupported-topology-format"
    | "malformed-element" | "semantic-ineligibility";
  reasons?: EligibilityReason[];
}

/** A stored digest format is absent (historical), canonical, or the Episode lineage format, whose version marker must agree with it. */
function supportedDigestFormat(format: string | number | null, version: string | number | null | undefined): format is string | null {
  return !((format !== null && format !== CANONICAL_DIGEST && format !== "episode-rfc8785-v2")
    || (version != null && version !== 2)
    || (format === "episode-rfc8785-v2") !== (version === 2));
}

interface DecodedElement { el: MemoryElement; payloadHash: string | null; previousRevisionKey: string | null }

/** The element as stored, decoded by its existing format; null when the row is malformed. */
function decodeStoredElement(p: ElementProperties, format: string | null): DecodedElement | null {
  try {
    const payloadHash = StoredHash.parse(p["payload_hash"] ?? null);
    const previousRevisionKey = StoredHash.parse(p["previous_revision_key"] ?? null);
    return { el: format === null ? decodeHistoricalElement(p) : toElement(p), payloadHash, previousRevisionKey };
  } catch (error) {
    if (!(error instanceof z.ZodError) && !(error instanceof SyntaxError)) throw error;
    return null;
  }
}

/** Every Episode's persisted NEXT_EPISODE edges must be the version-1 lattice its parents imply. */
function topologyIssues(rows: ReturnType<typeof topologyExpectations>): IntegrityIssue[] {
  const issues: IntegrityIssue[] = [];
  for (const row of rows) {
    if (row.version !== 1) {
      issues.push({ elementId: row.id, kind: "unsupported-topology-format" });
    } else if (row.actual.filter((edge) => edge !== null).length !== row.parents.length || row.parents.some((parent) =>
      !row.actual.some((edge) => edge !== null && edge.from === parent && edge.key === tupleHash([row.sessionKey, parent, row.id])))) {
      issues.push({ elementId: row.id, kind: "topology-mismatch" });
    }
  }
  return issues;
}

export class IntegrityStore {
  constructor(private readonly core: StoreCore, private readonly receipts: ReceiptStore, private readonly elements: ElementStore) {}

  /** The stored digest against the element as decoded, and for lineage-format Episodes the lineage chain as well. */
  private async digestIssuesTx(p: ElementProperties, decoded: DecodedElement, format: string | null, elementId: string): Promise<IntegrityIssue[]> {
    const { el, payloadHash, previousRevisionKey } = decoded, issues: IntegrityIssue[] = [];
    try {
      if (p["episode_digest_version"] === 2) {
        try { await this.core.withReadTx(tx => this.receipts.lineageTx(tx, elementId, String(p["lineage_digest"]))); }
        catch (error) {
          if (!(error instanceof EpisodeLineageError) && !(error instanceof z.ZodError) && !(error instanceof SyntaxError)) throw error;
          issues.push({ elementId, kind: "digest-mismatch" });
        }
      }
      if (elementDigest(el, { payloadHash, previousRevisionKey, format,
        episodeDigestVersion: p["episode_digest_version"] === 2 ? 2 : null,
        originRole: p["origin_role"] as string | null, lineageDigest: p["lineage_digest"] as string | null }) !== p["digest"]) {
        issues.push({ elementId: el.id, kind: "digest-mismatch" });
      }
    } catch (error) {
      if (!(error instanceof StorageContractError)) throw error;
      if (error.code !== "unsupported_digest_format" && error.code !== "invalid_canonical_json") throw error;
      issues.push({ elementId: el.id, kind: error.code === "unsupported_digest_format"
        ? "unsupported-digest-format" : "digest-mismatch" });
    }
    return issues;
  }
  /** The payload a stored element names: recorded in the graph, present as bytes, and hashing to its name. A historical
   * element's raw integrity must see damaged bytes, not ObjectStore.has()'s serving eligibility result (which also
   * rejects missing sidecars). */
  private async payloadIssuesTx(el: MemoryElement, elementId: string, payloadHash: string, format: string | null): Promise<IntegrityIssue[]> {
    const issues: IntegrityIssue[] = [];
    if (format === null) {
      const metadata = await this.core.run<{ hash: string }>(
        "MATCH (p:Payload {hash:$hash}) RETURN p.hash AS hash", { hash: payloadHash });
      if (!metadata.length) issues.push({ elementId, kind: "missing-payload" });
      try {
        const payload = await this.core.objects.get(payloadHash);
        if (sha256(payload) !== payloadHash) issues.push({ elementId, kind: "payload-hash-mismatch" });
      } catch (error) {
        if (!(error instanceof Error && "code" in error && error.code === "ENOENT")) throw error;
        if (metadata.length) issues.push({ elementId, kind: "missing-payload" });
      }
      return issues;
    }
    const payload = await this.elements.getPayload(payloadHash);
    if (!payload) issues.push({ elementId: el.id, kind: "missing-payload" });
    else if (sha256(payload) !== payloadHash) issues.push({ elementId: el.id, kind: "payload-hash-mismatch" });
    return issues;
  }
  async verify(): Promise<IntegrityIssue[]> {
    const issues: IntegrityIssue[] = [];
    const rows = await this.core.run<{ e: ElementNode }>(
      `MATCH (e:Element) RETURN e`,
    );
    for (const row of rows) {
      const p = nodeProps(row["e"]);
      const elementId = String(p["id"]);
      const format = p["digest_format"] ?? null;
      if (!supportedDigestFormat(format, p["episode_digest_version"])) { issues.push({ elementId, kind: "unsupported-digest-format" }); continue; }
      // Choose the existing stored format before validation, never as a fallback
      // from failed modern admission. No defaults enter the historical digest.
      const decoded = decodeStoredElement(p, format);
      if (!decoded) { issues.push({ elementId, kind: "malformed-element" }); continue; }
      if (format === null) {
        const reasons = historicalEligibility(decoded.el);
        if (reasons.length) issues.push({ elementId, kind: "semantic-ineligibility", reasons });
      }
      issues.push(...await this.digestIssuesTx(p, decoded, format, elementId));
      if (decoded.payloadHash) issues.push(...await this.payloadIssuesTx(decoded.el, elementId, decoded.payloadHash, format));
    }
    issues.push(...topologyIssues(topologyExpectations(await this.core.run<TopologyRow>(TOPOLOGY_QUERY))));
    return issues;
  }
  async counts(): Promise<{
    elements: number;
    links: number;
    pending: number;
  }> {
    const rows = await this.core.run<{
      elements: number;
      links: number;
      pending: number;
    }>(
      `CALL () { MATCH (e:Element) RETURN count(e) AS elements }
       CALL () { MATCH (:Element)-[l]->(:Element) RETURN count(l) AS links }
       CALL () { MATCH (o:Outbox) WHERE o.processed_at IS NULL
                 RETURN count(o) AS pending }
       RETURN elements, links, pending`,
    );
    const r = rows[0]!;
    return {
      elements: r.elements,
      links: r.links,
      pending: r.pending,
    };
  }
}
