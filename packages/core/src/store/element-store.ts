import neo4j, { type ManagedTransaction } from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { LINK_LATTICE, MemoryElement, MemoryLink, type MemoryElementInput, type MemoryLinkInput, type LinkRole, type TimePoint } from "@anamnesis/protocol";
import { EchoLineage, EpisodeLineageError, parseEpisodeLineage, type EpisodeLineageInput } from "@anamnesis/protocol";
import { canonicalExtractionBody, extractionBodyDigest } from "@anamnesis/protocol";
import { type TopologyRow, TOPOLOGY_QUERY, topologyExpectations } from "./conducting.ts";
import { CONDUCTING_ROLES, celestialOf, carriesTime, labelClause, toUtc } from "./schema.ts";
import { sha256, END_OF_TIME, CANONICAL_DIGEST, StorageContractError, elementDigest, verifyLineageRetry, tupleHash, originKey, sessionKey, linkIdemKey } from "./digest.ts";
import { luceneQuery, receiptTime } from "./receipts.ts";
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
}

export interface SearchHit {
  element: MemoryElement;

  score: number;
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
        const retry = async (): Promise<PutResult | null> => {
          const existing = await tx.run<{
            id: string;
            digest: string;
            format: string | number | null;
            previousRevisionKey: string | null;
            version: number | null; role: string | null; lineageDigest: string | null;
          }>(
            `MATCH (e:Element:Episode { revision_key: $revisionKey })
             RETURN e.id AS id, e.digest AS digest, e.digest_format AS format,
                    e.episode_digest_version AS version,e.origin_role AS role,e.lineage_digest AS lineageDigest,
                    e.previous_revision_key AS previousRevisionKey`,
            { revisionKey },
          );
          const record = existing.records[0];
          if (!record) return null;
          const version = record.get("version");
          if (version !== null && version !== 2) throw new EpisodeLineageError("unsupported_digest_version");
          if (version === 2) {
            const lineage = await this.receipts.lineageTx(tx, record.get("id"), record.get("lineageDigest"));
            verifyLineageRetry(opts.admission?.metadata, record.get("role"), lineage);
          }
          const candidateDigest = elementDigest(el, {
            episodeDigestVersion: version, originRole: record.get("role"), lineageDigest: record.get("lineageDigest"),
            payloadHash,
            previousRevisionKey: opts.expectedPreviousRevisionKey === undefined
              ? record.get("previousRevisionKey") : opts.expectedPreviousRevisionKey,
            format: record.get("format"),
          });
          if (record.get("digest") !== candidateDigest) {
            throw new StorageContractError("revision_conflict", revisionKey);
          }
          return { id: record.get("id"), created: false };
        };
        const existing = await retry();
        if (existing) return existing;
        const head = await tx.run<{ revisionKey: string | null }>(
          `MERGE (h:OriginHead { origin_key: $originKey })
           SET h.revision_key = h.revision_key
           RETURN h.revision_key AS revisionKey`,
          { originKey: key },
        );
        const previousRevisionKey = head.records[0]!.get("revisionKey") ?? null;
        // A contender may have committed this exact revision while we waited
        // for the unique head's write lock. Recheck before attempting CREATE.
        const raced = await retry();
        if (raced) return raced;
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
        await this.createElementTx(tx, el, payload, opts, {
          sourceRevision, revisionKey, previousRevisionKey, ingestedAt,
          ...(metadata ? { originRole: metadata.origin_role, lineageDigest: lineageDigest! } : {}),
          digest: elementDigest(el, { payloadHash, previousRevisionKey,
            ...(metadata ? { format: "episode-rfc8785-v2", episodeDigestVersion: 2, originRole: metadata.origin_role, lineageDigest } : {}) }),
        });
        if (lineage) await tx.run(`CREATE (l:EchoLineage $props)`, {
          props: { ...lineage, body: canonicalExtractionBody(lineage), digest: lineageDigest },
        });
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
        // Every remember contends for the single Meta node's write lock and
        // Neo4j holds it until commit, so the increment rides on the last
        // sequence-independent statement, after CREATE and originals links.
        // Topology depends on this sequence and runs under the same lock.
        // It stays inside this
        // transaction, so an aborted remember consumes no number.
        await tx.run(
          `MATCH (h:OriginHead { origin_key: $originKey })
           MATCH (e:Element:Episode { id: $id })
           SET h.revision_key = $revisionKey
           MERGE (m:Meta { key: 'meta' })
           ON CREATE SET m.ingest_seq = 0
           SET m.ingest_seq = m.ingest_seq + 1
           SET e.ingest_seq = m.ingest_seq`,
          { originKey: key, revisionKey, id: el.id },
        );
        await this.spliceTopologyTx(tx, { id: el.id, sessionKey: sessionKey(el.origin) });
        if (lineage) await tx.run(`MATCH (m:Meta {key:'meta'}) SET m.structure_revision=coalesce(m.structure_revision,0)+1`);
        return {
          id: el.id,
          created: true,
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
    opts: { enqueue?: boolean; previous?: string },
    revision?: ElementRevision,
  ): Promise<void> {
    if (payload) {
      await tx.run(
        `MERGE (p:Payload { hash: $hash })
         ON CREATE SET p.size = $size, p.media_type = $mediaType`,
        { hash: payload.hash, size: payload.size, mediaType: payload.mediaType },
      );
    }
    const isEpisode = celestialOf(el.schema) === "Episode";
    const time = carriesTime(el.schema) ? el.time ?? null : null;
    await tx.run(
      `CREATE (e:${labelClause(el.schema)} {
         id: $id, schema: $schema,
         time_value: $timeValue, time_utc: $timeUtc,
         time_precision: $timePrecision,
         content: $content,
         origin_key: $originKey, session_key: $sessionKey,
         origin_source: $source, origin_session: $session,
         origin_actor: $actor, origin_record: $record,
         mass: $mass, properties: $properties,
         payload_hash: $payloadHash, digest: $digest, digest_format: $digestFormat,
         episode_digest_version: $episodeDigestVersion, origin_role: $originRole, lineage_digest: $lineageDigest,
         topology_version: $topologyVersion, topology_previous_record: $previous,
         source_revision: $sourceRevision, revision_key: $revisionKey,
         previous_revision_key: $previousRevisionKey,
         ingest_seq: null, ingested_at: $ingestedAt
       })`,
      {
        originKey: originKey(el.origin),
        sessionKey: isEpisode ? sessionKey(el.origin) : null,
        id: el.id,
        schema: el.schema,
        ...timeColumns(time),
        content: el.content,
        source: el.origin.source,
        session: el.origin.session,
        actor: el.origin.actor,
        record: el.origin.record,
        mass: el.mass,
        properties: JSON.stringify(el.properties),
        payloadHash: payload?.hash ?? null,
        ...revisionColumns(revision, el, payload?.hash ?? null),
        topologyVersion: isEpisode ? 1 : null,
        previous: isEpisode ? opts.previous ?? null : null,
      },
    );
    if (payload) {
      await tx.run(
        `MATCH (e:Element:Episode { id: $id }), (p:Payload { hash: $hash })
         MERGE (e)-[:HAS_PAYLOAD]->(p)`,
        { id: el.id, hash: payload.hash },
      );
    }
    if (opts.enqueue) {
      await tx.run(
        `MATCH (e:Element { id: $id })
         CREATE (o:Outbox { element_id: $id, enqueued_at: $now,
                            processed_at: null })-[:OF]->(e)`,
        { id: el.id, now: new Date().toISOString() },
      );
    }
  }
  /** Meta's sequence lock is held until commit, serializing cache splices. */
  private async spliceTopologyTx(tx: ManagedTransaction, episode: { id: string; sessionKey: string }): Promise<void> {
    const { id, sessionKey } = episode;
    const predecessors = await tx.run<{ id: string }>(
      `MATCH (e:Element:Episode {id: $id}), (p:Element:Episode)
       WHERE p.session_key = e.session_key
         AND (p.time_utc < e.time_utc OR (p.time_utc = e.time_utc AND p.ingest_seq < e.ingest_seq))
       RETURN p.id AS id ORDER BY p.time_utc DESC, p.ingest_seq DESC LIMIT 1`, { id });
    const successors = await tx.run<{ id: string; previousRecord: string | null }>(
      `MATCH (e:Element:Episode {id: $id}), (s:Element:Episode)
       WHERE s.session_key = e.session_key
         AND (s.time_utc > e.time_utc OR (s.time_utc = e.time_utc AND s.ingest_seq > e.ingest_seq))
       RETURN s.id AS id, s.topology_previous_record AS previousRecord
       ORDER BY s.time_utc, s.ingest_seq LIMIT 1`, { id });
    const predecessor = predecessors.records[0]?.get("id") ?? null;
    const successor = successors.records[0];
    if (successor && successor.get("previousRecord") === null) {
      const removed = await tx.run<{ id: string }>(
        `MATCH (p:Element:Episode {id: $predecessor})-[l:NEXT_EPISODE]->(s:Element:Episode {id: $successor})
         RETURN l.id AS id`, { predecessor, successor: successor.get("id") });
      await this.conducting.deleteTopologyLinksTx(tx, removed.records.map(row => row.get("id")));
      await this.mergeTopologyTx(tx, { from: id, to: successor.get("id"), sessionKey });
    }
    const parents = await tx.run<{ id: string }>(
      `MATCH (e:Element:Episode {id: $id}), (p:Element:Episode)
       WHERE (e.topology_previous_record IS NULL AND p.id = $predecessor)
         OR (e.topology_previous_record IS NOT NULL AND p.session_key = e.session_key
             AND p.origin_record = e.topology_previous_record AND p.ingest_seq < e.ingest_seq)
       RETURN p.id AS id`, { id, predecessor });
    for (const parent of parents.records) {
      await this.mergeTopologyTx(tx, { from: parent.get("id"), to: id, sessionKey });
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
        ? ", target_id: b.id, effective_time_utc: a.time_utc"
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
  async searchText(
    query: string,
    opts: { limit?: number; until?: string; validOnly?: boolean } = {},
  ): Promise<SearchHit[]> {
    const q = luceneQuery(query);
    if (!q) return [];
    const rows = await this.core.run<{ e: ElementNode; score: number }>(
      `CALL db.index.fulltext.queryNodes('element_content', $q)
       YIELD node, score
       WHERE ($until IS NULL OR node.time_utc IS NULL
              OR node.time_utc <= $until)
         AND (NOT $validOnly OR NOT EXISTS {
           MATCH ()-[inv:INVALIDATES]->()
           WHERE inv.target_id = node.id
             AND inv.effective_time_utc <= coalesce($until, $endOfTime)
             AND inv.id IS NOT NULL
         })
       RETURN node AS e, score
       ORDER BY score DESC
       LIMIT $limit`,
      {
        q,
        until: opts.until ? toUtc(opts.until) : null,
        endOfTime: END_OF_TIME,
        validOnly: opts.validOnly ?? false,
        limit: neo4j.int(opts.limit ?? 20),
      },
    );
    return rows.map((r) => ({
      element: toElement(nodeProps(r["e"])),
      score: r.score,
    }));
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
