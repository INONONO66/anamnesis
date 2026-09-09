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
  TIME_BEARING,
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
  kind: "digest-mismatch" | "missing-payload" | "payload-hash-mismatch"
    | "unsupported-digest-format" | "topology-mismatch" | "unsupported-topology-format";
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
  expectedPreviousRevisionKey?: string | null;
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
  `CREATE CONSTRAINT origin_head_key IF NOT EXISTS
   FOR (h:OriginHead) REQUIRE h.origin_key IS UNIQUE`,
  `CREATE CONSTRAINT payload_hash IF NOT EXISTS
   FOR (p:Payload) REQUIRE p.hash IS UNIQUE`,
  // Without it, remembers racing on a cold database each MERGE their own Meta
  // node and hand out the same ingest_seq.
  `CREATE CONSTRAINT meta_key IF NOT EXISTS
   FOR (m:Meta) REQUIRE m.key IS UNIQUE`,
  `CREATE INDEX episode_origin IF NOT EXISTS
   FOR (e:Episode) ON (e.origin_key)`,
  `CREATE INDEX episode_session_order IF NOT EXISTS
   FOR (e:Episode) ON (e.session_key, e.time_utc, e.ingest_seq)`,
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

/** Used when no snapshot cutoff is given, so every invalidator applies. */
const END_OF_TIME = "9999-12-31T23:59:59.999Z";

const CANONICAL_DIGEST = "rfc8785-v1";

interface TopologyRow {
  id: string;
  sessionKey: string;
  record: string;
  previousRecord: string | null;
  timeUtc: string;
  ingestSeq: number;
  version: number | null;
  actual: ({ from: string; key: string | null } | null)[];
}

const TOPOLOGY_QUERY = `MATCH (e:Element:Episode)
  OPTIONAL MATCH (p)-[l:NEXT_EPISODE]->(e)
  RETURN e.id AS id, e.session_key AS sessionKey, e.origin_record AS record,
    e.topology_previous_record AS previousRecord, e.ingest_seq AS ingestSeq,
    e.topology_version AS version, e.time_utc AS timeUtc,
    collect(CASE WHEN l IS NULL THEN null ELSE {from: p.id, key: l.idem_key} END) AS actual
  ORDER BY sessionKey, timeUtc, ingestSeq`;

/** Derive expectations independently of the cache, retaining explicit-parent
 * semantics as observed at admission (later source revisions are not parents).
 */
function topologyExpectations(rows: TopologyRow[]): (TopologyRow & { parents: string[] })[] {
  const records = new Map<string, TopologyRow[]>();
  for (const row of rows) {
    const key = JSON.stringify([row.sessionKey, row.record]);
    const bucket = records.get(key) ?? [];
    bucket.push(row);
    records.set(key, bucket);
  }
  const previousBySession = new Map<string, string>();
  return rows.map((row) => {
    const previous = previousBySession.get(row.sessionKey);
    const parents = row.previousRecord === null
      ? previous === undefined ? [] : [previous]
      : (records.get(JSON.stringify([row.sessionKey, row.previousRecord])) ?? [])
        .filter((parent) => parent.ingestSeq < row.ingestSeq).map((parent) => parent.id);
    previousBySession.set(row.sessionKey, row.id);
    return { ...row, parents };
  });
}

class StorageContractError extends Error {
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
function canonicalJson(value: MemoryElement["properties"][string]): string {
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
}

function elementDigest(
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
  // Absent stored markers mean frozen insertion-ordered legacy bytes, never
  // an invitation to migrate or sort a previously admitted original.
  switch (context.format) {
    case null: return sha256(JSON.stringify(body));
    case undefined:
    case CANONICAL_DIGEST: return sha256(canonicalJson(body));
    default: throw new StorageContractError("unsupported_digest_format", String(context.format));
  }
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
  return c !== null && TIME_BEARING[c];
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
        const retry = async (): Promise<PutResult | null> => {
          const existing = await tx.run<{
            id: string;
            digest: string;
            format: string | number | null;
            previousRevisionKey: string | null;
          }>(
            `MATCH (e:Element:Episode { revision_key: $revisionKey })
             RETURN e.id AS id, e.digest AS digest, e.digest_format AS format,
                    e.previous_revision_key AS previousRevisionKey`,
            { revisionKey },
          );
          const record = existing.records[0];
          if (!record) return null;
          const candidateDigest = elementDigest(el, {
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
        await this.createElementTx(tx, el, payload, opts, {
          sourceRevision,
          revisionKey,
          previousRevisionKey,
          ingestedAt: Date.now(),
          digest: elementDigest(el, { payloadHash, previousRevisionKey }),
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
         payload_hash: $payloadHash, digest: $digest, digest_format: $digestFormat,
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
        digest: revision?.digest ?? elementDigest(el, { payloadHash: payload?.hash ?? null }),
        digestFormat: CANONICAL_DIGEST,
        topologyVersion: isEpisode ? 1 : null,
        previous: isEpisode ? opts.previous ?? null : null,
        sourceRevision: revision?.sourceRevision ?? null,
        revisionKey: revision?.revisionKey ?? null,
        previousRevisionKey: revision?.previousRevisionKey ?? null,
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
      await tx.run(
        `MATCH (p:Element:Episode {id: $predecessor})-[l:NEXT_EPISODE]->(s:Element:Episode {id: $successor})
         DELETE l`, { predecessor, successor: successor.get("id") });
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
    await tx.run(
      `MATCH (p:Element:Episode {id: $from}), (e:Element:Episode {id: $to})
       MERGE (p)-[l:NEXT_EPISODE]->(e)
       ON CREATE SET l.id = $linkId, l.idem_key = $idemKey,
         l.content = CASE WHEN e.topology_previous_record IS NULL
           THEN 'This is the next episode in the same session'
           ELSE 'This episode follows the explicitly selected parent record' END,
         l.weight = 1.0`,
      { ...edge, linkId: uuidv7(), idemKey: tupleHash([edge.sessionKey, edge.from, edge.to]) });
  }

  /** Only cache links are replaced. Legacy explicit parents were not persisted,
   * so rebuilding unmarked rows would invent provenance; require journal recovery.
   */
  async rebuildTopology(): Promise<void> {
    await this.withWriteTx(async (tx) => {
      await tx.run(`MERGE (m:Meta {key: 'meta'}) ON CREATE SET m.ingest_seq = 0
        SET m.ingest_seq = m.ingest_seq`);
      const result = await tx.run<TopologyRow>(TOPOLOGY_QUERY);
      const rows = topologyExpectations(recordsToObjects(result.records));
      for (const row of rows) {
        if (row.version !== 1) throw new StorageContractError("unsupported_topology_format", row.id);
      }
      await tx.run(`MATCH ()-[l:NEXT_EPISODE]->() DELETE l`);
      for (const row of rows) {
        for (const parent of row.parents) {
          await this.mergeTopologyTx(tx, { from: parent, to: row.id, sessionKey: row.sessionKey });
        }
      }
    });
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
        ? ", target_id: b.id, effective_time_utc: a.time_utc"
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
      try {
        if (elementDigest(el, { payloadHash, previousRevisionKey, format: p["digest_format"] ?? null }) !== p["digest"]) {
          issues.push({ elementId: el.id, kind: "digest-mismatch" });
        }
      } catch (error) {
        if (!(error instanceof StorageContractError)) throw error;
        if (error.code !== "unsupported_digest_format" && error.code !== "invalid_canonical_json") throw error;
        issues.push({ elementId: el.id, kind: error.code === "unsupported_digest_format"
          ? "unsupported-digest-format" : "digest-mismatch" });
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
    for (const row of topologyExpectations(await this.run<TopologyRow>(TOPOLOGY_QUERY))) {
      if (row.version !== 1) {
        issues.push({ elementId: row.id, kind: "unsupported-topology-format" });
      } else if (row.actual.filter((edge) => edge !== null).length !== row.parents.length || row.parents.some((parent) =>
        !row.actual.some((edge) => edge !== null && edge.from === parent && edge.key === tupleHash([row.sessionKey, parent, row.id])))) {
        issues.push({ elementId: row.id, kind: "topology-mismatch" });
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
