/**
 * Store — Neo4j 단일 스토어 (docs/02).
 *
 * 하나의 그래프에 에피소드(원본)와 가공 데이터(주장·매핑·통합·무효화)가
 * 함께 쌓인다. 기억 데이터(Element / LINK / Payload)에 대해 이 모듈은
 * **CREATE 계열 Cypher만 발행한다** — UPDATE/DELETE 경로 자체가 존재하지
 * 않는다 (불변성 규율, docs/02 §불변성).
 *
 * - 틀린 사실 → invalidation 사건 + invalidates 링크 (수정이 아니라 사건)
 * - 중요도 변화 → mass(T) 읽기 시점 평가
 * - 가변인 것은 커서(Outbox)뿐 — 기억 데이터가 아니다.
 *
 * 멱등성: origin 3-튜플을 origin_key로 접어 UNIQUE 제약 + MERGE ON CREATE.
 * 같은 것을 다시 넣으면 기존 원소를 그대로 돌려준다 (덮어쓰기 없음).
 *
 * 시간축: 모든 원소는 time_utc(UTC ISO8601)를 가진다. snapshot(T) 절단과
 * invalidates 판정이 전부 이 축 위에서 이뤄진다.
 */
import neo4j, { Driver } from "neo4j-driver";
import { createHash } from "node:crypto";
import {
  MemoryElement,
  MemoryLink,
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
  /** false면 이미 존재 (origin 멱등) */
  created: boolean;
}

export interface SearchHit {
  element: MemoryElement;
  /** Lucene score — 높을수록 관련도 높음 */
  score: number;
}

export interface IntegrityIssue {
  elementId: string;
  kind: "digest-mismatch" | "missing-payload" | "payload-hash-mismatch";
}

const SCHEMA_STATEMENTS = [
  `CREATE CONSTRAINT element_id IF NOT EXISTS
   FOR (e:Element) REQUIRE e.id IS UNIQUE`,
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
  // CJK 분석기 — 한국어 bigram 매칭 ("커피를"에서 "커피" 검색 가능)
  `CREATE FULLTEXT INDEX element_content IF NOT EXISTS
   FOR (e:Element) ON EACH [e.content]
   OPTIONS { indexConfig: { \`fulltext.analyzer\`: 'cjk' } }`,
];

function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

/** 정규화 필드 고정 순서의 canonical digest */
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

function toUtc(isoWithOffset: string): string {
  return new Date(isoWithOffset).toISOString();
}

/** Lucene 질의 문법 주입 방지 — 특수문자 제거 후 단순 term 질의로 */
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

  /** 제약·인덱스 보장. 엔진 기동 시 1회. */
  async init(): Promise<void> {
    for (const stmt of SCHEMA_STATEMENTS) await this.run(stmt);
    // fulltext 인덱스가 온라인 될 때까지 대기 (기동 직후 질의 보호)
    await this.run(`CALL db.awaitIndexes(60)`);
  }

  /**
   * 유일한 원소 쓰기 경로. origin 멱등 — 이미 있으면 기존 원소 반환,
   * 어떤 프로퍼티도 덮어쓰지 않는다 (ON CREATE만 존재).
   */
  async putElement(
    input: unknown,
    opts: { payload?: Uint8Array; enqueue?: boolean } = {},
  ): Promise<PutResult> {
    const el = MemoryElement.parse(input);
    const payloadHash = opts.payload ? sha256(opts.payload) : null;

    if (opts.payload && payloadHash) {
      await this.run(
        `MERGE (p:Payload { hash: $hash })
         ON CREATE SET p.bytes = $bytes`,
        { hash: payloadHash, bytes: Buffer.from(opts.payload).toString("base64") },
      );
    }

    const rows = await this.run(
      `MERGE (e:Element { origin_key: $originKey })
       ON CREATE SET e += {
         id: $id, schema: $schema,
         time_value: $timeValue, time_utc: $timeUtc,
         time_precision: $timePrecision,
         content: $content,
         origin_source: $source, origin_session: $session,
         origin_actor: $actor, origin_record: $record,
         mass: $mass, properties: $properties,
         payload_hash: $payloadHash, digest: $digest
       }
       RETURN e.id AS id`,
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
    const id = rows[0]!["id"] as string;
    const created = id === el.id;

    if (created && opts.enqueue) {
      await this.run(
        `MATCH (e:Element { id: $id })
         CREATE (o:Outbox { element_id: $id, enqueued_at: $now,
                            processed_at: null })-[:OF]->(e)`,
        { id, now: new Date().toISOString() },
      );
    }
    return { id, created };
  }

  /** 링크 쓰기 — id 멱등, ON CREATE만 (덮어쓰기 없음) */
  async putLink(input: unknown): Promise<MemoryLink> {
    const link = MemoryLink.parse(input);
    const rows = await this.run(
      `MATCH (a:Element { id: $from }), (b:Element { id: $to })
       MERGE (a)-[l:LINK { id: $id }]->(b)
       ON CREATE SET l += { role: $role, content: $content, weight: $weight }
       RETURN l.id AS id`,
      { ...link },
    );
    if (rows.length === 0) {
      throw new Error(`link endpoints not found: ${link.from} -> ${link.to}`);
    }
    return link;
  }

  async getElement(id: string): Promise<MemoryElement | null> {
    const rows = await this.run(
      `MATCH (e:Element { id: $id }) RETURN e`,
      { id },
    );
    return rows.length ? toElement(nodeProps(rows[0]!["e"])) : null;
  }

  async getPayload(hash: string): Promise<Uint8Array | null> {
    const rows = await this.run(
      `MATCH (p:Payload { hash: $hash }) RETURN p.bytes AS bytes`,
      { hash },
    );
    return rows.length
      ? new Uint8Array(Buffer.from(rows[0]!["bytes"] as string, "base64"))
      : null;
  }

  /**
   * 전문검색 (Lucene, CJK). until = snapshot(T) 절단 — 그 시점 이후의
   * 기억은 존재하지 않았던 것으로 취급한다.
   */
  async searchText(
    query: string,
    opts: { limit?: number; until?: string } = {},
  ): Promise<SearchHit[]> {
    const q = luceneQuery(query);
    if (!q) return [];
    const rows = await this.run(
      `CALL db.index.fulltext.queryNodes('element_content', $q)
       YIELD node, score
       WHERE $until IS NULL OR node.time_utc <= $until
       RETURN node AS e, score
       ORDER BY score DESC
       LIMIT $limit`,
      {
        q,
        until: opts.until ? toUtc(opts.until) : null,
        limit: neo4j.int(opts.limit ?? 20),
      },
    );
    return rows.map((r) => ({
      element: toElement(nodeProps(r["e"])),
      score: r["score"] as number,
    }));
  }

  async linksOf(id: string, role?: LinkRole): Promise<MemoryLink[]> {
    const rows = await this.run(
      `MATCH (a:Element)-[l:LINK]-(b:Element)
       WHERE (a.id = $id OR b.id = $id)
         AND ($role IS NULL OR l.role = $role)
       WITH DISTINCT l, startNode(l) AS s, endNode(l) AS t
       RETURN l, s.id AS from, t.id AS to`,
      { id, role: role ?? null },
    );
    return rows.map((r) => {
      const p = relProps(r["l"]);
      return MemoryLink.parse({
        id: p["id"],
        from: r["from"],
        to: r["to"],
        role: p["role"],
        content: p["content"],
        weight: p["weight"],
      });
    });
  }

  /**
   * 시간축 유효성 판정:
   * valid(fact, T) = fact.time ≤ T ∧ ¬∃ inv: (inv)-[invalidates]->(fact) ∧ inv.time ≤ T
   */
  async isValidAt(id: string, at: string): Promise<boolean> {
    const atUtc = toUtc(at);
    const rows = await this.run(
      `MATCH (e:Element { id: $id })
       RETURN e.time_utc <= $at
              AND NOT EXISTS {
                MATCH (inv:Element)-[l:LINK { role: 'invalidates' }]->(e)
                WHERE inv.time_utc <= $at
              } AS valid`,
      { id, at: atUtc },
    );
    return rows.length > 0 && rows[0]!["valid"] === true;
  }

  /** 콜드패스 커서 — UUIDv7 정렬이라 시간순 */
  async pending(limit = 100): Promise<string[]> {
    const rows = await this.run(
      `MATCH (o:Outbox) WHERE o.processed_at IS NULL
       RETURN o.element_id AS id ORDER BY id LIMIT $limit`,
      { limit: neo4j.int(limit) },
    );
    return rows.map((r) => r["id"] as string);
  }

  async markProcessed(elementIds: string[]): Promise<void> {
    await this.run(
      `MATCH (o:Outbox) WHERE o.element_id IN $ids
       SET o.processed_at = $now`,
      { ids: elementIds, now: new Date().toISOString() },
    );
  }

  /** 재추출용 — 커서만 다시 올린다. 기억 데이터는 불변. */
  async requeue(schema: string): Promise<number> {
    const rows = await this.run(
      `MATCH (e:Element { schema: $schema })
       CREATE (o:Outbox { element_id: e.id, enqueued_at: $now,
                          processed_at: null })-[:OF]->(e)
       RETURN count(o) AS n`,
      { schema, now: new Date().toISOString() },
    );
    return rows[0]!["n"] as number;
  }

  /** 전수 무결성 감사 — digest 재계산 + payload 해시 검증 */
  async verify(): Promise<IntegrityIssue[]> {
    const issues: IntegrityIssue[] = [];
    const rows = await this.run(`MATCH (e:Element) RETURN e`);
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
    const rows = await this.run(
      `CALL () { MATCH (e:Element) RETURN count(e) AS elements }
       CALL () { MATCH ()-[l:LINK]->() RETURN count(l) AS links }
       CALL () { MATCH (o:Outbox) WHERE o.processed_at IS NULL
                 RETURN count(o) AS pending }
       RETURN elements, links, pending`,
    );
    const r = rows[0]!;
    return {
      elements: r["elements"] as number,
      links: r["links"] as number,
      pending: r["pending"] as number,
    };
  }

  async close(): Promise<void> {
    await this.driver.close();
  }

  private async run(
    cypher: string,
    params: Record<string, unknown> = {},
  ): Promise<Record<string, unknown>[]> {
    const res = await this.driver.executeQuery(cypher, params, {
      database: this.database,
    });
    return res.records.map((rec) => Object.fromEntries(
      rec.keys.map((k) => [k as string, rec.get(k)]),
    ));
  }
}

function nodeProps(node: unknown): Record<string, unknown> {
  return (node as { properties: Record<string, unknown> }).properties;
}

function relProps(rel: unknown): Record<string, unknown> {
  return (rel as { properties: Record<string, unknown> }).properties;
}

function toElement(p: Record<string, unknown>): MemoryElement {
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
    properties: JSON.parse((p["properties"] as string) ?? "{}"),
  });
}
