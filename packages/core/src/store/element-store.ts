import { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { LINK_LATTICE, MemoryElement, MemoryLink, type MemoryElementInput, type MemoryLinkInput, type LinkRole, type TimePoint } from "@anamnesis/protocol";
import { EchoLineage, EpisodeLineageError, parseEpisodeLineage, type EpisodeLineageInput } from "@anamnesis/protocol";
import { extractionBodyDigest } from "@anamnesis/protocol";
import { type TopologyRow, TOPOLOGY_QUERY, topologyExpectations } from "./conducting.ts";
import { CONDUCTING_ROLES, celestialOf, carriesTime, labelClause, toUtc } from "./schema.ts";
import { sha256, CANONICAL_DIGEST, StorageContractError, elementDigest, verifyLineageRetry, tupleHash, originKey, sessionKey, linkIdemKey } from "./digest.ts";
import { receiptTime } from "./receipts.ts";
import { requireInstallation } from "./policy.ts";
import { type ElementNode, type LinkRelationship, type ElementWriteOptions, recordsToObjects, nodeProps, relProps, toElement } from "./records.ts";
import type { StoreCore } from "./core.ts";
import type { ReceiptStore } from "./receipt-store.ts";
import type { ConductingStore } from "./conducting-store.ts";

export interface PutResult {
  id: string;

  created: boolean;

  diverged?: boolean;

  invalidated?: string;

  /** Set when an Episode was created: the sequence this transaction assigned. */
  ingest_seq?: number;
}

interface ElementRevision {
  sourceRevision: string;
  revisionKey: string;
  previousRevisionKey: string | null;
  ingestedAt: number;
  digest: string;
  originRole?: string;
  lineageDigest?: string;
}

const timeColumns = (time: TimePoint | null) => ({ timeValue: time?.value ?? null, timeUtc: time ? toUtc(time.value) : null, timePrecision: time?.precision ?? null });

/** Revision columns of an Episode row; an element without a revision carries its own canonical digest and the current ingest time. */
function revisionColumns(revision: ElementRevision | undefined, el: MemoryElement, payloadHash: string | null) {
  if (!revision) {
    return { digest: elementDigest(el, { payloadHash }), digestFormat: CANONICAL_DIGEST, episodeDigestVersion: null, originRole: null, lineageDigest: null,
      sourceRevision: null, revisionKey: null, previousRevisionKey: null, ingestedAt: Date.now() };
  }
  return { digest: revision.digest, digestFormat: revision.lineageDigest ? "episode-rfc8785-v2" : CANONICAL_DIGEST, episodeDigestVersion: revision.lineageDigest ? 2 : null,
    originRole: revision.originRole ?? null, lineageDigest: revision.lineageDigest ?? null, sourceRevision: revision.sourceRevision, revisionKey: revision.revisionKey,
    previousRevisionKey: revision.previousRevisionKey, ingestedAt: revision.ingestedAt };
}

/** Every stored column of a new element row; ingest_seq is assigned separately, under Meta's lock. */
function elementColumns(el: MemoryElement, payloadHash: string | null, previous: string | undefined, revision: ElementRevision | undefined) {
  const isEpisode = celestialOf(el.schema) === "Episode";
  const { timeValue, timeUtc, timePrecision } = timeColumns(carriesTime(el.schema) ? el.time ?? null : null);
  const stored = revisionColumns(revision, el, payloadHash);
  return {
    id: el.id, schema: el.schema,
    time_value: timeValue, time_utc: timeUtc, time_precision: timePrecision,
    content: el.content,
    origin_key: originKey(el.origin), session_key: isEpisode ? sessionKey(el.origin) : null,
    origin_source: el.origin.source, origin_session: el.origin.session,
    origin_actor: el.origin.actor, origin_record: el.origin.record,
    mass: el.mass, properties: JSON.stringify(el.properties),
    payload_hash: payloadHash, digest: stored.digest, digest_format: stored.digestFormat,
    episode_digest_version: stored.episodeDigestVersion, origin_role: stored.originRole, lineage_digest: stored.lineageDigest,
    topology_version: isEpisode ? 1 : null, topology_previous_record: isEpisode ? previous ?? null : null,
    source_revision: stored.sourceRevision, revision_key: stored.revisionKey,
    previous_revision_key: stored.previousRevisionKey,
    ingest_seq: null, ingested_at: stored.ingestedAt,
  };
}

type RevisionLookup = {
  id: string | null;
  digest: string | null;
  format: string | number | null;
  previousRevisionKey: string | null;
  version: number | null; role: string | null; lineageDigest: string | null;
  heads: string[];
};

function lineageColumns({ episode_id: _episodeId, complete, ...columns }: EchoLineage) {
  return { ...columns, lineage_complete: complete };
}

export class ElementStore {
  constructor(private readonly core: StoreCore, private readonly receipts: ReceiptStore, private readonly conducting: ConductingStore) {}

  async putElement(
    input: MemoryElementInput,
    opts: ElementWriteOptions = {},
  ): Promise<PutResult> {
    return this.putParsedElement(MemoryElement.parse(input), opts);
  }
  /** Engine ingestion uses this after its derived input schema has parsed once. */
  async putParsedElement(
    el: MemoryElement,
    opts: ElementWriteOptions = {},
  ): Promise<PutResult> {
    const payload = opts.payload
      ? await this.core.objects.put(
          opts.payload,
          opts.payloadMediaType ?? "application/octet-stream",
        )
      : null;
    const payloadHash = payload?.hash ?? null;
    const now = new Date().toISOString();
    const celestial = celestialOf(el.schema);
    const sourceRevision = opts.sourceRevision ?? el.origin.record;
    if (celestial === "Episode") {
      return this.core.withWriteTx(async (tx) => {
        const key = originKey(el.origin);
        const revisionKey = tupleHash([key, sourceRevision]);
        // The stored revision and the origin's head arrive in one round trip; the head matters only to a new revision.
        const lookup = await tx.run<RevisionLookup>(
          `OPTIONAL MATCH (e:Element:Episode { revision_key: $revisionKey })
           CALL {
             MATCH (head:Episode {origin_key:$originKey})
             USING INDEX SEEK head:Episode(origin_key, ingest_seq)
             WHERE head.ingest_seq IS NOT NULL
             WITH head ORDER BY head.origin_key DESC, head.ingest_seq DESC LIMIT 1
             RETURN collect(head.revision_key) AS heads
           }
           RETURN e.id AS id, e.digest AS digest, e.digest_format AS format,
                  e.episode_digest_version AS version,e.origin_role AS role,e.lineage_digest AS lineageDigest,
                  e.previous_revision_key AS previousRevisionKey, heads`,
          { revisionKey, originKey: key },
        );
        const stored = lookup.records[0]!.toObject();
        if (stored.id !== null) return this.storedRevisionTx(tx, el, opts, payloadHash, revisionKey, stored.id, stored);
        const previousRevisionKey = stored.heads[0] ?? null;
        if (opts.expectedPreviousRevisionKey !== undefined &&
            opts.expectedPreviousRevisionKey !== previousRevisionKey) {
          throw new StorageContractError("stale_revision", revisionKey);
        }
        const prior = previousRevisionKey
          ? await tx.run<{ id: string }>(
              `MATCH (e:Element:Episode { revision_key: $revisionKey })
               RETURN e.id AS id`,
              { revisionKey: previousRevisionKey },
            )
          : null;
        const previousId = prior?.records[0]?.get("id") ?? null;
        if (previousRevisionKey !== null && previousId === null) {
          throw new StorageContractError("stale_revision", previousRevisionKey);
        }
        // One logical tick accommodates a parent issued in the same clock ms.
        const ingestedAt = receiptTime.parse(this.core.clock() + 1);
        let lineage: EchoLineage | undefined, metadata: EpisodeLineageInput | undefined;
        if (opts.admission) {
          requireInstallation(opts.admission.context);
          if (this.core.writerEpoch === undefined) throw new Error("writer_epoch_required");
          metadata = parseEpisodeLineage(opts.admission.metadata);
          lineage = await this.receipts.admitLineageTx(tx, el.id, metadata, opts.admission.context, ingestedAt);
        }
        const lineageDigest = lineage ? extractionBodyDigest(lineage) : null;
        const slot = await this.createEpisodeTx(tx, el, payload, opts, {
          sourceRevision, revisionKey, previousRevisionKey, ingestedAt,
          ...(metadata ? { originRole: metadata.origin_role, lineageDigest: lineageDigest! } : {}),
          digest: elementDigest(el, { payloadHash, previousRevisionKey,
            ...(metadata ? { format: "episode-rfc8785-v2", episodeDigestVersion: 2, originRole: metadata.origin_role, lineageDigest } : {}) }),
        }, lineage);
        if (previousId) {
          await this.mergeLinkTx(tx, {
            id: uuidv7(),
            from: el.id,
            to: previousId,
            role: "INVALIDATES",
            content: "This source revision supersedes the previous revision",
            weight: 1,
          });
        }
        await this.spliceTopologyTx(tx, { id: el.id, sessionKey: sessionKey(el.origin), ...slot });
        return {
          id: el.id,
          created: true,
          ingest_seq: slot.seq,
          ...(previousId ? { invalidated: previousId } : {}),
        };
      });
    }

    return this.core.withWriteTx(async (tx) => {
      const existing = await tx.run<{
        id: string;
        content: string;
        schema: string;
      }>(
        `MATCH (e:Element { origin_key: $originKey })
           RETURN e.id AS id, e.content AS content, e.schema AS schema`,
        { originKey: originKey(el.origin) },
      );

      if (existing.records.length > 0) {
        const rec = existing.records[0]!;
        const oldId = rec.get("id");
        const oldContent = rec.get("content");
        if (sha256(oldContent) === sha256(el.content)) {
          return { id: oldId, created: false };
        }

        const hash8 = sha256(el.content).slice(0, 8);
        const derivedRecord = `${el.origin.record}#h${hash8}`;
        const dup = await tx.run<{ id: string }>(
          `MATCH (e:Element { origin_key: $originKey }) RETURN e.id AS id`,
          {
            originKey: originKey({ ...el.origin, record: derivedRecord }),
          },
        );
        if (dup.records.length > 0) {
          return {
            id: dup.records[0]!.get("id"),
            created: false,
            diverged: true,
          };
        }
        const divergedEl = {
          ...el,
          id: uuidv7(),
          origin: { ...el.origin, record: derivedRecord },
          properties: { ...el.properties, diverged_at: now },
        };
        await this.createElementTx(tx, divergedEl, payload, opts);
        const oldCelestial = celestialOf(rec.get("schema"));
        const invalidated =
          LINK_LATTICE.INVALIDATES.from.some((label) => label === celestial) &&
          LINK_LATTICE.INVALIDATES.to.some((label) => label === oldCelestial)
            ? oldId
            : null;
        if (!invalidated) {
          return { id: divergedEl.id, created: true, diverged: true };
        }
        await this.mergeLinkTx(tx, {
          id: uuidv7(),
          from: divergedEl.id,
          to: invalidated,
          role: "INVALIDATES",
          content:
            "Different content at the same origin was detected as a divergence",
          weight: 1,
        });
        return {
          id: divergedEl.id,
          created: true,
          diverged: true,
          invalidated,
        };
      }

      await this.createElementTx(tx, el, payload, opts);
      return { id: el.id, created: true };
    });
  }
  async createElementTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    payload: { hash: string; size: number; mediaType: string } | null,
    opts: { previous?: string; existingFact?: boolean },
    revision?: ElementRevision,
  ): Promise<void> {
    if (payload) await this.mergePayloadTx(tx, payload);
    await tx.run(
      `${opts.existingFact ? "MATCH (e:Fact {id:$id}) SET e:Element" : `CREATE (e:${labelClause(el.schema)})`}
       SET e += $columns`,
      { id: el.id, columns: elementColumns(el, payload?.hash ?? null, opts.previous, revision) },
    );
    if (payload) await this.linkPayloadTx(tx, el.id, payload.hash);
  }
  /** A revision already stored answers the remember only when its lineage and core digest verify against this delivery. */
  private async storedRevisionTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    opts: ElementWriteOptions,
    payloadHash: string | null,
    revisionKey: string,
    id: string,
    stored: RevisionLookup,
  ): Promise<PutResult> {
    if (stored.version !== null && stored.version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
    if (stored.version === 2) {
      const lineage = await this.receipts.lineageTx(tx, id, stored.lineageDigest);
      verifyLineageRetry(opts.admission?.metadata, stored.role, lineage);
    }
    const candidateDigest = elementDigest(el, {
      episodeDigestVersion: stored.version, originRole: stored.role, lineageDigest: stored.lineageDigest,
      payloadHash,
      previousRevisionKey: opts.expectedPreviousRevisionKey === undefined
        ? stored.previousRevisionKey : opts.expectedPreviousRevisionKey,
      format: stored.format,
    });
    if (stored.digest !== candidateDigest) {
      throw new StorageContractError("revision_conflict", revisionKey);
    }
    return { id, created: false };
  }
  /** Creates an Episode row with its lineage columns and assigns the next ingest_seq. The transaction's fence
   * already holds the single Meta node's write lock until commit, which serializes the indexed head lookup, CAS,
   * Episode creation and sequence assignment: an aborted remember can neither publish a head nor consume a sequence. */
  private async createEpisodeTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    payload: { hash: string; size: number; mediaType: string } | null,
    opts: { previous?: string },
    revision: ElementRevision,
    lineage: EchoLineage | undefined,
  ): Promise<{ t: string; seq: number; previous: string | null }> {
    if (payload) await this.mergePayloadTx(tx, payload);
    const created = await tx.run<{ t: string; seq: number; previous: string | null }>(
      `CREATE (e:${labelClause(el.schema)}) SET e += $columns
       WITH e MERGE (m:Meta { key: 'meta' })
       ON CREATE SET m.ingest_seq = 0
       SET m.ingest_seq = m.ingest_seq + 1
       SET e.ingest_seq = m.ingest_seq
       ${lineage ? "SET m.structure_revision = coalesce(m.structure_revision, 0) + 1" : ""}
       RETURN e.time_utc AS t, e.ingest_seq AS seq, e.topology_previous_record AS previous`,
      { columns: { ...elementColumns(el, payload?.hash ?? null, opts.previous, revision), ...(lineage ? lineageColumns(lineage) : {}) } },
    );
    if (payload) await this.linkPayloadTx(tx, el.id, payload.hash);
    const row = created.records[0]!;
    return { t: row.get("t"), seq: row.get("seq"), previous: row.get("previous") };
  }
  private async mergePayloadTx(tx: ManagedTransaction, payload: { hash: string; size: number; mediaType: string }): Promise<void> {
    await tx.run(
      `MERGE (p:Payload { hash: $hash })
       ON CREATE SET p.size = $size, p.media_type = $mediaType`,
      { hash: payload.hash, size: payload.size, mediaType: payload.mediaType },
    );
  }
  private async linkPayloadTx(tx: ManagedTransaction, id: string, hash: string): Promise<void> {
    await tx.run(
      `MATCH (e:Element:Episode { id: $id }), (p:Payload { hash: $hash })
       MERGE (e)-[:HAS_PAYLOAD]->(p)`,
      { id, hash },
    );
  }
  /** Meta's sequence lock is held until commit, serializing cache splices. */
  private async spliceTopologyTx(tx: ManagedTransaction, episode: { id: string; sessionKey: string; t: string; seq: number; previous: string | null }): Promise<void> {
    const { id, sessionKey, t, seq, previous } = episode;
    const key = { sk: sessionKey, t, seq };
    // A disjunction over (time_utc, ingest_seq) makes the planner union a whole-label scan per
    // remember, so each neighbour is two seeks on episode_session_order: same instant first (it
    // outranks any other instant), then the nearest other instant; all four share one round trip.
    // Inside CALL subqueries a plain USING INDEX let the planner pick a whole-index scan on fresh
    // statistics (PROFILE: 60,011 db hits on a 30k-Episode session); SEEK forbids it. The hint needs
    // a predicate on every indexed property; ingest_seq is assigned before the splice, so IS NOT NULL
    // is free. Ordering by every index property lets each seek stop at its first entry (12 db hits).
    const neighbours = await tx.run<{ predecessor: string | null; successor: string | null; successorPrevious: string | null }>(
      `CALL { MATCH (p:Element:Episode) USING INDEX SEEK p:Episode(session_key, time_utc, ingest_seq)
         WHERE p.session_key = $sk AND p.time_utc = $t AND p.ingest_seq < $seq
         WITH p ORDER BY p.session_key DESC, p.time_utc DESC, p.ingest_seq DESC LIMIT 1 RETURN collect(p.id) AS sameBefore }
       CALL { MATCH (p:Element:Episode) USING INDEX SEEK p:Episode(session_key, time_utc, ingest_seq)
         WHERE p.session_key = $sk AND p.time_utc < $t AND p.ingest_seq IS NOT NULL
         WITH p ORDER BY p.session_key DESC, p.time_utc DESC, p.ingest_seq DESC LIMIT 1 RETURN collect(p.id) AS before }
       CALL { MATCH (s:Element:Episode) USING INDEX SEEK s:Episode(session_key, time_utc, ingest_seq)
         WHERE s.session_key = $sk AND s.time_utc = $t AND s.ingest_seq > $seq
         WITH s ORDER BY s.session_key, s.time_utc, s.ingest_seq LIMIT 1 RETURN collect(s {.id, .topology_previous_record}) AS sameAfter }
       CALL { MATCH (s:Element:Episode) USING INDEX SEEK s:Episode(session_key, time_utc, ingest_seq)
         WHERE s.session_key = $sk AND s.time_utc > $t AND s.ingest_seq IS NOT NULL
         WITH s ORDER BY s.session_key, s.time_utc, s.ingest_seq LIMIT 1 RETURN collect(s {.id, .topology_previous_record}) AS after }
       WITH coalesce(sameBefore[0], before[0]) AS predecessor, coalesce(sameAfter[0], after[0]) AS successor
       RETURN predecessor, successor.id AS successor, successor.topology_previous_record AS successorPrevious`, key);
    const found = neighbours.records[0]!;
    const predecessor = found.get("predecessor"), successor = found.get("successor");
    if (successor !== null && found.get("successorPrevious") === null) {
      const removed = await tx.run<{ id: string }>(
        `MATCH (p:Element:Episode {id: $predecessor})-[l:NEXT_EPISODE]->(s:Element:Episode {id: $successor})
         RETURN l.id AS id`, { predecessor, successor });
      await this.conducting.deleteTopologyLinksTx(tx, removed.records.map(row => row.get("id")));
      await this.mergeTopologyTx(tx, { from: id, to: successor, sessionKey });
    }
    // Episodes always carry time, so the time_utc predicate below only completes the index hint.
    const parents = previous === null
      ? predecessor === null ? [] : [predecessor]
      : (await tx.run<{ id: string }>(
        `MATCH (p:Element:Episode) USING INDEX p:Episode(session_key, time_utc, ingest_seq)
         WHERE p.session_key = $sk AND p.time_utc IS NOT NULL AND p.ingest_seq < $seq AND p.origin_record = $previous
         RETURN p.id AS id`, { ...key, previous })).records.map(row => row.get("id"));
    for (const parent of parents) {
      await this.mergeTopologyTx(tx, { from: parent, to: id, sessionKey });
    }
  }
  private async mergeTopologyTx(tx: ManagedTransaction, edge: { from: string; to: string; sessionKey: string }): Promise<void> {
    const result = await tx.run<{ id: string }>(
      `MATCH (p:Element:Episode {id: $from}), (e:Element:Episode {id: $to})
       MERGE (p)-[l:NEXT_EPISODE]->(e)
       ON CREATE SET l.id = $linkId, l.idem_key = $idemKey,
         l.content = CASE WHEN e.topology_previous_record IS NULL
           THEN 'This is the next episode in the same session'
           ELSE 'This episode follows the explicitly selected parent record' END,
         l.weight = 1.0
       RETURN l.id AS id`,
      { ...edge, linkId: uuidv7(), idemKey: tupleHash([edge.sessionKey, edge.from, edge.to]) });
    if (result.summary.counters.updates().relationshipsCreated > 0) {
      await this.conducting.appendConductingTx(tx, "NEXT_EPISODE", result.records[0]!.get("id"));
    }
  }
  /** Only cache links are replaced. Legacy explicit parents were not persisted,
   * so rebuilding unmarked rows would invent provenance; require journal recovery.
   */
  async rebuildTopology(): Promise<void> {
    await this.core.withWriteTx(async (tx) => {
      await tx.run(`MERGE (m:Meta {key: 'meta'}) ON CREATE SET m.ingest_seq = 0
        SET m.ingest_seq = m.ingest_seq`);
      const result = await tx.run<TopologyRow>(TOPOLOGY_QUERY);
      const rows = topologyExpectations(recordsToObjects(result.records));
      for (const row of rows) {
        if (row.version !== 1) throw new StorageContractError("unsupported_topology_format", row.id);
      }
      const removed = await tx.run<{ id: string }>(`MATCH ()-[l:NEXT_EPISODE]->() RETURN l.id AS id`);
      await this.conducting.deleteTopologyLinksTx(tx, removed.records.map(row => row.get("id")));
      for (const row of rows) {
        for (const parent of row.parents) {
          await this.mergeTopologyTx(tx, { from: parent, to: row.id, sessionKey: row.sessionKey });
        }
      }
    });
  }
  async putLink(input: MemoryLinkInput): Promise<MemoryLink> {
    const link = MemoryLink.parse(input);
    return this.core.withWriteTx(async (tx) => {
      const rows = await this.mergeLinkTx(tx, link);
      if (rows.length === 0) {
        throw new Error(
          `link rejected (endpoints missing or lattice violation): ` +
            `${link.from} -[${link.role}]-> ${link.to}`,
        );
      }
      return { ...link, id: rows[0]!.id };
    });
  }
  async mergeLinkTx(
    tx: ManagedTransaction,
    link: MemoryLink,
  ): Promise<{ id: string }[]> {
    const lattice = LINK_LATTICE[link.role];
    // INVALIDATES copies the seek fields of docs/01 §5 so valid(T) resolves
    // without expanding an incoming adjacency list. An Episode→Episode
    // revision edge belongs to the originals layer and keys without content.
    const seek =
      link.role === "INVALIDATES"
        ? ", target_id: b.id, effective_time_utc: a.time_utc, ingested_at: a.ingested_at"
        : "";
    const res = await tx.run<{ id: string }>(
      `MATCH (a:Element { id: $from }), (b:Element { id: $to })
       WHERE any(x IN labels(a) WHERE x IN $fromLabels)
         AND any(x IN labels(b) WHERE x IN $toLabels)
       WITH a, b, CASE WHEN $originalsLayer AND a:Episode AND b:Episode
                    THEN $originalsIdemKey ELSE $derivedIdemKey END AS key
       MERGE (a)-[l:${link.role} { idem_key: key }]->(b)
       ON CREATE SET l += { id: $id, content: $content, weight: $weight${seek} },
         l.generation = CASE WHEN $role = 'NEXT_EPISODE' OR (a:Episode AND b:Episode)
           THEN null ELSE coalesce(a.generation,b.generation) END
       RETURN l.id AS id`,
      {
        from: link.from,
        to: link.to,
        fromLabels: [...lattice.from],
        toLabels: [...lattice.to],
        originalsLayer: link.role === "INVALIDATES",
        originalsIdemKey: linkIdemKey(link, true),
        derivedIdemKey: linkIdemKey(link, false),
        id: link.id,
        content: link.content,
        weight: link.weight,
        role: link.role,
      },
    );
    if (res.summary.counters.updates().relationshipsCreated > 0 && CONDUCTING_ROLES.some(role => role === link.role)) {
      await this.conducting.appendConductingTx(tx, link.role, res.records[0]!.get("id"));
    }
    return recordsToObjects(res.records);
  }
  async getElement(id: string): Promise<MemoryElement | null> {
    const rows = await this.core.run<{ e: ElementNode }>(
      `MATCH (e:Element { id: $id }) RETURN e`,
      { id },
    );
    return rows.length ? toElement(nodeProps(rows[0]!["e"])) : null;
  }
  async getPayload(hash: string): Promise<Uint8Array | null> {
    const rows = await this.core.run<{ hash: string }>(
      `MATCH (p:Payload { hash: $hash }) RETURN p.hash AS hash`,
      { hash },
    );
    return rows.length && await this.core.objects.has(hash) ? this.core.objects.get(hash) : null;
  }
  async linksOf(id: string, role?: LinkRole): Promise<MemoryLink[]> {
    const rows = await this.core.run<{
      l: LinkRelationship;
      role: LinkRole;
      from: string;
      to: string;
    }>(
      `MATCH (a:Element)-[l]-(b:Element)
       WHERE (a.id = $id OR b.id = $id)
         AND ($role IS NULL OR type(l) = $role)
       WITH DISTINCT l, startNode(l) AS s, endNode(l) AS t
       RETURN l, type(l) AS role, s.id AS from, t.id AS to`,
      { id, role: role ?? null },
    );
    return rows.map((r) => {
      const p = relProps(r["l"]);
      return MemoryLink.parse({
        id: p["id"],
        from: r.from,
        to: r.to,
        role: r.role,
        content: p["content"],
        weight: p["weight"],
      });
    });
  }
  async isValidAt(id: string, at: string): Promise<boolean> {
    const atUtc = toUtc(at);
    const rows = await this.core.run<{ valid: boolean }>(
      `MATCH (e:Element { id: $id })
       RETURN (e.time_utc IS NULL OR e.time_utc <= $at)
              AND NOT EXISTS {
                MATCH ()-[inv:INVALIDATES]->()
                WHERE inv.target_id = e.id
                  AND inv.effective_time_utc <= $at
                  AND inv.id IS NOT NULL
              } AS valid`,
      { id, at: atUtc },
    );
    return rows.length > 0 && rows[0]!.valid;
  }
}
