import neo4j, {
  Driver,
  type ManagedTransaction,
  type Node,
  type Record as Neo4jRecord,
  type RecordShape,
  type Relationship,
} from "neo4j-driver";
import { createHash } from "node:crypto";
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

export interface StoreOptions {
  uri: string;
  user: string;
  password: string;
  database?: string;
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

const LINK_ROLES = Object.keys(LINK_LATTICE) as LinkRole[];

const SCHEMA_STATEMENTS = [
  `CREATE CONSTRAINT element_id IF NOT EXISTS
   FOR (e:Element) REQUIRE e.id IS UNIQUE`,

  ...LINK_ROLES.map(
    (role) => `CREATE CONSTRAINT link_idem_${role.toLowerCase()} IF NOT EXISTS
   FOR ()-[l:${role}]-() REQUIRE l.idem_key IS UNIQUE`,
  ),
  `CREATE CONSTRAINT element_origin IF NOT EXISTS
   FOR (e:Element) REQUIRE e.origin_key IS UNIQUE`,
  `CREATE CONSTRAINT payload_hash IF NOT EXISTS
   FOR (p:Payload) REQUIRE p.hash IS UNIQUE`,
  `CREATE INDEX element_time IF NOT EXISTS
   FOR (e:Element) ON (e.time_utc)`,
  `CREATE INDEX element_schema IF NOT EXISTS
   FOR (e:Element) ON (e.schema)`,
  `CREATE INDEX outbox_pending IF NOT EXISTS
   FOR (o:Outbox) ON (o.processed_at)`,

  `CREATE FULLTEXT INDEX element_content IF NOT EXISTS
   FOR (e:Element) ON EACH [e.content]
   OPTIONS { indexConfig: { \`fulltext.analyzer\`: 'cjk' } }`,
];

function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

function elementDigest(e: {
  schema: string;
  time: TimePoint;
  content: string;
  origin: Origin;
}): string {
  return sha256(
    JSON.stringify([
      e.schema,
      e.time.value,
      e.time.precision,
      e.content,
      e.origin.source,
      e.origin.session,
      e.origin.actor,
      e.origin.record,
    ]),
  );
}

function originKey(o: Origin): string {
  return [o.source, o.session, o.record].join("\u0000");
}

function linkIdemKey(l: {
  from: string;
  to: string;
  role: string;
  content: string;
}): string {
  return sha256([l.from, l.to, l.role, l.content].join("\u0000"));
}

function celestialOf(schema: string): Celestial | null {
  return SCHEMA_LABELS[schema as KnownSchema] ?? null;
}

function labelClause(schema: string): string {
  const c = celestialOf(schema);
  return c ? `Element:${c}` : "Element";
}

function toUtc(isoWithOffset: string): string {
  return new Date(isoWithOffset).toISOString();
}

function luceneQuery(raw: string): string {
  return raw
    .replace(/[+\-&|!(){}\[\]^"~*?:\\\/]/g, " ")
    .split(/\s+/)
    .filter(Boolean)
    .join(" ");
}

export class Store {
  private readonly driver: Driver;
  private readonly database: string;

  constructor(opts: StoreOptions) {
    this.driver = neo4j.driver(
      opts.uri,
      neo4j.auth.basic(opts.user, opts.password),
      { disableLosslessIntegers: true },
    );
    this.database = opts.database ?? "neo4j";
  }

  async init(): Promise<void> {
    for (const stmt of SCHEMA_STATEMENTS) await this.run(stmt);

    await this.run(`CALL db.awaitIndexes(60)`);
  }

  async putElement(
    input: MemoryElementInput,
    opts: { payload?: Uint8Array; enqueue?: boolean; previous?: string } = {},
  ): Promise<PutResult> {
    return this.putParsedElement(MemoryElement.parse(input), opts);
  }

  /** Engine ingestion uses this after its derived input schema has parsed once. */
  async putParsedElement(
    el: MemoryElement,
    opts: { payload?: Uint8Array; enqueue?: boolean; previous?: string } = {},
  ): Promise<PutResult> {
    const payloadHash = opts.payload ? sha256(opts.payload) : null;
    const now = new Date().toISOString();
    const celestial = celestialOf(el.schema);

    const session = this.driver.session({ database: this.database });
    try {
      return await session.executeWrite(async (tx) => {

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
          await this.createElementTx(tx, divergedEl, payloadHash, opts);
          const oldCelestial = celestialOf(rec.get("schema"));
          const invalidated =
            celestial &&
            oldCelestial &&
            LINK_LATTICE.INVALIDATES.from.includes(celestial) &&
            LINK_LATTICE.INVALIDATES.to.includes(oldCelestial)
              ? oldId
              : null;
          if (invalidated) {
            await this.mergeLinkTx(tx, {
              id: uuidv7(),
              from: divergedEl.id,
              to: invalidated,
              role: "INVALIDATES",
              content:
                "Different content at the same origin was detected as a divergence",
              weight: 1,
            });
          }
          return {
            id: divergedEl.id,
            created: true,
            diverged: true,
            ...(invalidated ? { invalidated } : {}),
          };
        }

        await this.createElementTx(tx, el, payloadHash, opts);
        return { id: el.id, created: true };
      });
    } finally {
      await session.close();
    }
  }

  private async createElementTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    payloadHash: string | null,
    opts: { payload?: Uint8Array; enqueue?: boolean; previous?: string },
  ): Promise<void> {
    if (opts.payload) {
      await tx.run(
        `MERGE (p:Payload { hash: $hash })
         ON CREATE SET p.bytes = $bytes`,
        { hash: payloadHash, bytes: Buffer.from(opts.payload).toString("base64") },
      );
    }
    await tx.run(
      `CREATE (e:${labelClause(el.schema)} {
         id: $id, schema: $schema,
         time_value: $timeValue, time_utc: $timeUtc,
         time_precision: $timePrecision,
         content: $content,
         origin_key: $originKey,
         origin_source: $source, origin_session: $session,
         origin_actor: $actor, origin_record: $record,
         mass: $mass, properties: $properties,
         payload_hash: $payloadHash, digest: $digest
       })`,
      {
        originKey: originKey(el.origin),
        id: el.id,
        schema: el.schema,
        timeValue: el.time.value,
        timeUtc: toUtc(el.time.value),
        timePrecision: el.time.precision,
        content: el.content,
        source: el.origin.source,
        session: el.origin.session,
        actor: el.origin.actor,
        record: el.origin.record,
        mass: el.mass,
        properties: JSON.stringify(el.properties),
        payloadHash,
        digest: elementDigest(el),
      },
    );
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
    const session = this.driver.session({ database: this.database });
    try {
      return await session.executeWrite(async (tx) => {
        const rows = await this.mergeLinkTx(tx, link);
        if (rows.length === 0) {
          throw new Error(
            `link rejected (endpoints missing or lattice violation): ` +
              `${link.from} -[${link.role}]-> ${link.to}`,
          );
        }
        return { ...link, id: rows[0]!.id };
      });
    } finally {
      await session.close();
    }
  }

  private async mergeLinkTx(
    tx: ManagedTransaction,
    link: MemoryLink,
  ): Promise<{ id: string }[]> {
    const lattice = LINK_LATTICE[link.role];
    const res = await tx.run<{ id: string }>(
      `MATCH (a:Element { id: $from }), (b:Element { id: $to })
       WHERE any(x IN labels(a) WHERE x IN $fromLabels)
         AND any(x IN labels(b) WHERE x IN $toLabels)
       MERGE (a)-[l:${link.role} { idem_key: $idemKey }]->(b)
       ON CREATE SET l += { id: $id, content: $content, weight: $weight }
       RETURN l.id AS id`,
      {
        from: link.from,
        to: link.to,
        fromLabels: [...lattice.from],
        toLabels: [...lattice.to],
        idemKey: linkIdemKey(link),
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
    const rows = await this.run<{ bytes: string }>(
      `MATCH (p:Payload { hash: $hash }) RETURN p.bytes AS bytes`,
      { hash },
    );
    return rows.length
      ? new Uint8Array(Buffer.from(rows[0]!.bytes, "base64"))
      : null;
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
       WHERE ($until IS NULL OR node.time_utc <= $until)
         AND (NOT $validOnly OR NOT EXISTS {
           MATCH (inv:Element)-[:INVALIDATES]->(node)
           WHERE $until IS NULL OR inv.time_utc <= $until
         })
       RETURN node AS e, score
       ORDER BY score DESC
       LIMIT $limit`,
      {
        q,
        until: opts.until ? toUtc(opts.until) : null,
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
       RETURN e.time_utc <= $at
              AND NOT EXISTS {
                MATCH (inv:Element)-[:INVALIDATES]->(e)
                WHERE inv.time_utc <= $at
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
      if (elementDigest(el) !== p["digest"]) {
        issues.push({ elementId: el.id, kind: "digest-mismatch" });
      }
      const payloadHash = p["payload_hash"] as string | null;
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

  async counts(): Promise<{ elements: number; links: number; pending: number }> {
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
    time: { value: p["time_value"], precision: p["time_precision"] },
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
