import neo4j, {
  Driver,
  type ManagedTransaction,
  type Node,
  type Record as Neo4jRecord,
  type RecordShape,
  type Relationship,
} from "neo4j-driver";
import { createHash } from "node:crypto";
import { homedir } from "node:os";
import { join } from "node:path";
import { v7 as uuidv7 } from "uuid";
import {
  LINK_LATTICE,
  MemoryElement,
  MemoryLink,
  SCHEMA_LABELS,
  type MemoryElementInput,
  type MemoryLinkInput,
  type Celestial,
  type KnownSchema,
  type LinkRole,
  type Origin,
  type TimePoint,
} from "@anamnesis/protocol";
import { ObjectStore } from "./objects.ts";

export interface StoreOptions {
  uri: string;
  user: string;
  password: string;
  database?: string;
  objectsRoot?: string;
}

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

export interface IntegrityIssue {
  elementId: string;
  kind: "digest-mismatch" | "missing-payload" | "payload-hash-mismatch";
}

type ElementProperties = Record<string, string | number | null>;
type LinkProperties = Record<string, string | number>;
type ElementNode = Node<number, ElementProperties>;
type LinkRelationship = Relationship<number, LinkProperties>;
type QueryParameter =
  | string
  | number
  | boolean
  | null
  | Buffer
  | string[]
  | ReturnType<typeof neo4j.int>;
type QueryParameters = Record<string, QueryParameter>;

interface ElementWriteOptions {
  payload?: Uint8Array;
  payloadMediaType?: string;
  sourceRevision?: string;
  enqueue?: boolean;
  previous?: string;
}

const LINK_ROLES = Object.keys(LINK_LATTICE) as LinkRole[];

const SCHEMA_STATEMENTS = [
  `CREATE CONSTRAINT element_id IF NOT EXISTS
   FOR (e:Element) REQUIRE e.id IS UNIQUE`,

  ...LINK_ROLES.map(
    (role) => `CREATE CONSTRAINT link_idem_${role.toLowerCase()} IF NOT EXISTS
   FOR ()-[l:${role}]-() REQUIRE l.idem_key IS UNIQUE`,
  ),
  `CREATE CONSTRAINT episode_revision IF NOT EXISTS
   FOR (e:Episode) REQUIRE e.revision_key IS UNIQUE`,
  `CREATE CONSTRAINT episode_ingest_seq IF NOT EXISTS
   FOR (e:Episode) REQUIRE e.ingest_seq IS UNIQUE`,
  `CREATE CONSTRAINT payload_hash IF NOT EXISTS
   FOR (p:Payload) REQUIRE p.hash IS UNIQUE`,
  `CREATE INDEX episode_origin IF NOT EXISTS
   FOR (e:Episode) ON (e.origin_key)`,
  `CREATE INDEX element_time IF NOT EXISTS
   FOR (e:Element) ON (e.time_utc)`,
  `CREATE INDEX element_schema IF NOT EXISTS
   FOR (e:Element) ON (e.schema)`,
  `CREATE INDEX outbox_pending IF NOT EXISTS
   FOR (o:Outbox) ON (o.processed_at)`,
  // valid(T) seeks invalidators by target instead of expanding adjacency.
  `CREATE INDEX invalidates_seek IF NOT EXISTS
   FOR ()-[l:INVALIDATES]-() ON (l.target_id, l.effective_time_utc, l.id)`,

  `CREATE FULLTEXT INDEX element_content IF NOT EXISTS
   FOR (e:Element) ON EACH [e.content]
   OPTIONS { indexConfig: { \`fulltext.analyzer\`: 'cjk' } }`,
];

function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

/**
 * A timeless invalidator is a retroactive correction (docs/03 §5): the target
 * was never valid at any T. Storing the lower bound instead of null keeps
 * `effective_time_utc` non-null so valid(T) stays a composite index seek.
 */
const BEGINNING_OF_TIME = "0000-01-01T00:00:00.000Z";
/** Used when no snapshot cutoff is given, so every invalidator applies. */
const END_OF_TIME = "9999-12-31T23:59:59.999Z";

function elementDigest(
  e: {
    schema: string;
    time?: TimePoint | undefined;
    content: string;
    properties?: MemoryElement["properties"] | undefined;
  },
  payloadHash: string | null = null,
  previousRevisionKey: string | null = null,
): string {
  return sha256(
    JSON.stringify({
      schema: e.schema,
      content: e.content,
      properties: Object.fromEntries(
        Object.entries(e.properties ?? {}).filter(
          ([key]) => key !== "payload_hash",
        ),
      ),
      time: carriesTime(e.schema) ? e.time ?? null : null,
      payload_hash: payloadHash,
      previous_revision_key: previousRevisionKey,
    }),
  );
}

function tupleHash(parts: readonly string[]): string {
  return sha256(JSON.stringify(parts));
}

function originKey(o: Origin): string {
  return tupleHash([o.source, o.session, o.actor, o.record]);
}

function sessionKey(o: Origin): string {
  return tupleHash([o.source, o.session]);
}

/** Originals-layer links are content-free keys; derived links bind content. */
function linkIdemKey(
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

function celestialOf(schema: string): Celestial | null {
  return SCHEMA_LABELS[schema as KnownSchema] ?? null;
}

/** Only Episode and Fact carry an event time (docs/03 §1). */
function carriesTime(schema: string): boolean {
  const c = celestialOf(schema);
  return c === "Episode" || c === "Fact";
}

function labelClause(schema: string): string {
  const c = celestialOf(schema);
  return c ? `Element:${c}` : "Element";
}

function toUtc(isoWithOffset: string): string {
  return new Date(isoWithOffset).toISOString();
}

export function luceneQuery(raw: string): string {
  return raw
    .replace(/[+\-&|!(){}\[\]^"~*?:\\\/]/g, " ")
    .split(/\s/)
    .filter(Boolean)
    .join(" ");
}

export class Store {
  private readonly driver: Driver;
  private readonly database: string;
  private readonly objects: ObjectStore;

  constructor(opts: StoreOptions, driver?: Driver) {
    this.driver =
      driver ??
      neo4j.driver(opts.uri, neo4j.auth.basic(opts.user, opts.password), {
        disableLosslessIntegers: true,
      });
    this.database = opts.database ?? "neo4j";
    this.objects = new ObjectStore(
      opts.objectsRoot ?? join(homedir(), ".anamnesis", "objects"),
    );
  }

  get databaseName(): string {
    return this.database;
  }

  async init(): Promise<void> {
    for (const stmt of SCHEMA_STATEMENTS) await this.run(stmt);

    await this.run(`CALL db.awaitIndexes(60)`);
  }

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
      ? await this.objects.put(
          opts.payload,
          opts.payloadMediaType ?? "application/octet-stream",
        )
      : null;
    const payloadHash = payload?.hash ?? null;
    const now = new Date().toISOString();
    const celestial = celestialOf(el.schema);
    const sourceRevision = opts.sourceRevision ?? el.origin.record;
    if (celestial === "Episode") {
      return this.withWriteTx(async (tx) => {
        const key = originKey(el.origin);
        const revisionKey = tupleHash([key, sourceRevision]);
        const existing = await tx.run<{
          id: string;
          digest: string;
          previousRevisionKey: string | null;
        }>(
          `MATCH (e:Element:Episode { revision_key: $revisionKey })
           RETURN e.id AS id, e.digest AS digest,
                  e.previous_revision_key AS previousRevisionKey`,
          { revisionKey },
        );
        if (existing.records.length > 0) {
          const record = existing.records[0]!;
          const candidateDigest = elementDigest(
            el,
            payloadHash,
            record.get("previousRevisionKey"),
          );
          if (record.get("digest") !== candidateDigest) {
            throw new Error(`revision_conflict: ${revisionKey}`);
          }
          return { id: record.get("id"), created: false };
        }
        const head = await tx.run<{ revisionKey: string | null }>(
          `MERGE (h:OriginHead { origin_key: $originKey })
           SET h.revision_key = h.revision_key
           RETURN h.revision_key AS revisionKey`,
          { originKey: key },
        );
        const previousRevisionKey = head.records[0]!.get("revisionKey") ?? null;
        const prior = previousRevisionKey
          ? await tx.run<{ id: string }>(
              `MATCH (e:Element:Episode { revision_key: $revisionKey })
               RETURN e.id AS id`,
              { revisionKey: previousRevisionKey },
            )
          : null;
        const previousId = prior?.records[0]?.get("id") ?? null;
        const sequence = await tx.run<{ value: number }>(
          `MERGE (m:Meta { key: 'meta' })
           ON CREATE SET m.ingest_seq = 0
           SET m.ingest_seq = m.ingest_seq + 1
           RETURN m.ingest_seq AS value`,
        );
        await this.createElementTx(tx, el, payload, opts, {
          sourceRevision,
          revisionKey,
          previousRevisionKey,
          ingestSeq: sequence.records[0]!.get("value"),
          ingestedAt: Date.now(),
          digest: elementDigest(el, payloadHash, previousRevisionKey),
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
        await tx.run(
          `MATCH (h:OriginHead { origin_key: $originKey })
           SET h.revision_key = $revisionKey`,
          { originKey: key, revisionKey },
        );
        return {
          id: el.id,
          created: true,
          ...(previousId ? { invalidated: previousId } : {}),
        };
      });
    }

    return this.withWriteTx(async (tx) => {
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

  private async createElementTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    payload: { hash: string; size: number; mediaType: string } | null,
    opts: { enqueue?: boolean; previous?: string },
    revision?: {
      sourceRevision: string;
      revisionKey: string;
      previousRevisionKey: string | null;
      ingestSeq: number;
      ingestedAt: number;
      digest: string;
    },
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
         payload_hash: $payloadHash, digest: $digest,
         source_revision: $sourceRevision, revision_key: $revisionKey,
         previous_revision_key: $previousRevisionKey,
         ingest_seq: $ingestSeq, ingested_at: $ingestedAt
       })`,
      {
        originKey: originKey(el.origin),
        sessionKey: isEpisode ? sessionKey(el.origin) : null,
        id: el.id,
        schema: el.schema,
        timeValue: time?.value ?? null,
        timeUtc: time ? toUtc(time.value) : null,
        timePrecision: time?.precision ?? null,
        content: el.content,
        source: el.origin.source,
        session: el.origin.session,
        actor: el.origin.actor,
        record: el.origin.record,
        mass: el.mass,
        properties: JSON.stringify(el.properties),
        payloadHash: payload?.hash ?? null,
        digest: revision?.digest ?? elementDigest(el),
        sourceRevision: revision?.sourceRevision ?? null,
        revisionKey: revision?.revisionKey ?? null,
        previousRevisionKey: revision?.previousRevisionKey ?? null,
        ingestSeq: revision?.ingestSeq ?? null,
        ingestedAt: Date.now(),
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

    if (celestialOf(el.schema) === "Episode") {
      if (opts.previous) {
        await tx.run(
          `MATCH (e:Element { id: $id })
           MATCH (p:Element:Episode)
           WHERE p.origin_source = $source AND p.origin_session = $session
             AND p.origin_record = $previous
           MERGE (p)-[l:NEXT_EPISODE]->(e)
           ON CREATE SET l += { id: $linkId, idem_key: $idemKey,
             content: 'This episode follows the explicitly selected parent record', weight: 1.0 }`,
          {
            id: el.id,
            source: el.origin.source,
            session: el.origin.session,
            previous: opts.previous,
            linkId: uuidv7(),
            idemKey: sha256(`next_episode:${el.id}`),
          },
        );
      } else {
        await tx.run(
          `MATCH (e:Element { id: $id })
           MATCH (p:Element:Episode)
           WHERE p.schema = e.schema
             AND p.origin_source = e.origin_source
             AND p.origin_session = e.origin_session
             AND p.id <> e.id
             AND (p.time_utc < e.time_utc
                  OR (p.time_utc = e.time_utc AND p.id < e.id))
           WITH e, p ORDER BY p.time_utc DESC, p.id DESC LIMIT 1
           MERGE (p)-[l:NEXT_EPISODE]->(e)
           ON CREATE SET l += { id: $linkId, idem_key: $idemKey,
             content: 'This is the next episode in the same session',
             weight: 1.0 }`,
          {
            id: el.id,
            linkId: uuidv7(),
            idemKey: sha256(`next_episode:${el.id}`),
          },
        );
      }
    }
  }

  async putLink(input: MemoryLinkInput): Promise<MemoryLink> {
    const link = MemoryLink.parse(input);
    return this.withWriteTx(async (tx) => {
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

  private async withWriteTx<Result>(
    work: (tx: ManagedTransaction) => Promise<Result>,
  ): Promise<Result> {
    const session = this.driver.session({ database: this.database });
    try {
      return await session.executeWrite(work);
    } finally {
      await session.close();
    }
  }

  private async mergeLinkTx(
    tx: ManagedTransaction,
    link: MemoryLink,
  ): Promise<{ id: string }[]> {
    const lattice = LINK_LATTICE[link.role];
    // INVALIDATES copies the seek fields of docs/01 §5 so valid(T) resolves
    // without expanding an incoming adjacency list. An Episode→Episode
    // revision edge belongs to the originals layer and keys without content.
    const seek =
      link.role === "INVALIDATES"
        ? ", target_id: b.id," +
          " effective_time_utc: coalesce(a.time_utc, $beginningOfTime)"
        : "";
    const res = await tx.run<{ id: string }>(
      `MATCH (a:Element { id: $from }), (b:Element { id: $to })
       WHERE any(x IN labels(a) WHERE x IN $fromLabels)
         AND any(x IN labels(b) WHERE x IN $toLabels)
       WITH a, b, CASE WHEN $originalsLayer AND a:Episode AND b:Episode
                    THEN $originalsIdemKey ELSE $derivedIdemKey END AS key
       MERGE (a)-[l:${link.role} { idem_key: key }]->(b)
       ON CREATE SET l += { id: $id, content: $content, weight: $weight${seek} }
       RETURN l.id AS id`,
      {
        from: link.from,
        to: link.to,
        fromLabels: [...lattice.from],
        toLabels: [...lattice.to],
        originalsLayer: link.role === "INVALIDATES",
        originalsIdemKey: linkIdemKey(link, true),
        derivedIdemKey: linkIdemKey(link, false),
        beginningOfTime: BEGINNING_OF_TIME,
        id: link.id,
        content: link.content,
        weight: link.weight,
      },
    );
    return recordsToObjects(res.records);
  }

  async getElement(id: string): Promise<MemoryElement | null> {
    const rows = await this.run<{ e: ElementNode }>(
      `MATCH (e:Element { id: $id }) RETURN e`,
      { id },
    );
    return rows.length ? toElement(nodeProps(rows[0]!["e"])) : null;
  }

  async getPayload(hash: string): Promise<Uint8Array | null> {
    const rows = await this.run<{ hash: string }>(
      `MATCH (p:Payload { hash: $hash }) RETURN p.hash AS hash`,
      { hash },
    );
    return rows.length && await this.objects.has(hash) ? this.objects.get(hash) : null;
  }

  async searchText(
    query: string,
    opts: { limit?: number; until?: string; validOnly?: boolean } = {},
  ): Promise<SearchHit[]> {
    const q = luceneQuery(query);
    if (!q) return [];
    const rows = await this.run<{ e: ElementNode; score: number }>(
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
    const rows = await this.run<{
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
    const rows = await this.run<{ valid: boolean }>(
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

  async pending(limit = 100): Promise<string[]> {
    const rows = await this.run<{ id: string }>(
      `MATCH (o:Outbox) WHERE o.processed_at IS NULL
       RETURN o.element_id AS id ORDER BY id LIMIT $limit`,
      { limit: neo4j.int(limit) },
    );
    return rows.map((r) => r.id);
  }

  async markProcessed(elementIds: string[]): Promise<void> {
    await this.run(
      `MATCH (o:Outbox) WHERE o.element_id IN $ids
       SET o.processed_at = $now`,
      { ids: elementIds, now: new Date().toISOString() },
    );
  }

  async requeue(schema: string): Promise<number> {
    const rows = await this.run<{ n: number }>(
      `MATCH (e:Element { schema: $schema })
       CREATE (o:Outbox { element_id: e.id, enqueued_at: $now,
                          processed_at: null })-[:OF]->(e)
       RETURN count(o) AS n`,
      { schema, now: new Date().toISOString() },
    );
    return rows[0]!.n;
  }

  async verify(): Promise<IntegrityIssue[]> {
    const issues: IntegrityIssue[] = [];
    const rows = await this.run<{ e: ElementNode }>(
      `MATCH (e:Element) RETURN e`,
    );
    for (const row of rows) {
      const p = nodeProps(row["e"]);
      const el = toElement(p);
      const payloadHash = p["payload_hash"] as string | null;
      const previousRevisionKey = (p["previous_revision_key"] as string | null) ?? null;
      if (elementDigest(el, payloadHash, previousRevisionKey) !== p["digest"]) {
        issues.push({ elementId: el.id, kind: "digest-mismatch" });
      }
      if (payloadHash) {
        const payload = await this.getPayload(payloadHash);
        if (!payload) {
          issues.push({ elementId: el.id, kind: "missing-payload" });
        } else if (sha256(payload) !== payloadHash) {
          issues.push({ elementId: el.id, kind: "payload-hash-mismatch" });
        }
      }
    }
    return issues;
  }

  async counts(): Promise<{
    elements: number;
    links: number;
    pending: number;
  }> {
    const rows = await this.run<{
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

  async close(): Promise<void> {
    await this.driver.close();
  }

  private async run<Row extends RecordShape>(
    cypher: string,
    params: QueryParameters = {},
  ): Promise<Row[]> {
    const res = await this.driver.executeQuery<Row>(cypher, params, {
      database: this.database,
    });
    return recordsToObjects(res.records);
  }
}

function recordsToObjects<Row extends RecordShape>(
  records: Neo4jRecord<Row>[],
): Row[] {
  return records.map((record) => record.toObject());
}

function nodeProps(node: ElementNode): ElementProperties {
  return node.properties;
}

function relProps(rel: LinkRelationship): LinkProperties {
  return rel.properties;
}

function toElement(p: ElementProperties): MemoryElement {
  return MemoryElement.parse({
    id: p["id"],
    schema: p["schema"],
    ...(p["time_value"] ? { time: { value: p["time_value"], precision: p["time_precision"] } } : {}),
    content: p["content"],
    origin: {
      source: p["origin_source"],
      session: p["origin_session"],
      actor: p["origin_actor"],
      record: p["origin_record"],
    },
    mass: p["mass"],
    properties: {
      ...JSON.parse((p["properties"] as string) ?? "{}"),
      ...(p["payload_hash"] ? { payload_hash: p["payload_hash"] } : {}),
    },
  });
}
