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
 * 멱등성 (2단, docs/01 §유입 의미론):
 * - 같은 origin 키 + 같은 내용 해시 = 진짜 재유입 → no-op.
 * - 같은 origin 키 + 다른 내용 해시 = 분기(divergence) → 새 원소
 *   (record에 #h<해시8> 접미) + 자동 INVALIDATES. 버리지 않고 보존.
 * - 링크의 멱등 키는 (from, to, role, content 해시) — 추출 재실행 안전.
 *
 * 라벨: 공통 :Element + 천체 라벨(:Episode/:Entity/:Fact/:Community)
 * 이중 물질화. 엣지는 격자(LINK_LATTICE) 밖이면 거부된다.
 *
 * 원자성: putElement는 단일 트랜잭션이다 — Payload·Element·Outbox·
 * NEXT_EPISODE 배선이 전부 함께 커밋되거나 전부 롤백된다 (docs/04).
 *
 * 시간축: 모든 원소는 time_utc(UTC ISO8601)를 가진다. snapshot(T) 절단과
 * invalidates 판정이 전부 이 축 위에서 이뤄진다.
 */
import neo4j, { Driver, type ManagedTransaction } from "neo4j-driver";
import { createHash } from "node:crypto";
import { v7 as uuidv7 } from "uuid";
import {
  LINK_LATTICE,
  MemoryElement,
  MemoryLink,
  SCHEMA_LABELS,
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
  /** false면 이미 존재 (origin 멱등) */
  created: boolean;
  /** 같은 origin, 다른 내용 → 분기로 처리됨 (docs/01 2단 멱등) */
  diverged?: boolean;
  /** 분기가 자동 INVALIDATES로 무효화한 기존 원소 id */
  invalidated?: string;
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

const LINK_ROLES = Object.keys(LINK_LATTICE) as LinkRole[];

const SCHEMA_STATEMENTS = [
  `CREATE CONSTRAINT element_id IF NOT EXISTS
   FOR (e:Element) REQUIRE e.id IS UNIQUE`,
  // 링크 멱등 키 (from, to, role, content 해시) — 관계 실타입별 unique
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

function linkIdemKey(l: {
  from: string;
  to: string;
  role: string;
  content: string;
}): string {
  return sha256([l.from, l.to, l.role, l.content].join("\u0000"));
}

/** schema → 천체 라벨. 레지스트리 밖 schema는 null (:Element만). */
function celestialOf(schema: string): Celestial | null {
  return SCHEMA_LABELS[schema as KnownSchema] ?? null;
}

/** Cypher 라벨 절 — 우리 맵에서만 보간하므로 안전하다. */
function labelClause(schema: string): string {
  const c = celestialOf(schema);
  return c ? `Element:${c}` : "Element";
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
   * 유일한 원소 쓰기 경로. 단일 트랜잭션 (docs/04 — 부분 유입 상태 불가).
   *
   * 2단 멱등 (docs/01 §유입 의미론):
   * - 같은 origin 키 + 같은 내용 해시 → no-op (기존 원소 반환).
   * - 같은 origin 키 + 다른 내용 해시 → 분기: 새 원소를 record
   *   `#h<내용해시8>` 접미로 만들고, 격자가 허용하면 (새것)-[:INVALIDATES]
   *   ->(기존) 자동 배선. 분기 관측 시각은 properties.diverged_at에만
   *   기록 — 사건 시각으로 승격하지 않는다.
   *
   * 신규 Episode 생성 시 NEXT_EPISODE 배선도 같은 트랜잭션에서:
   * ① opts.previous(부모 record 명시) 우선 ② 폴백: 같은 (source,
   * session, schema)에서 사건 시각이 직전인 에피소드 (나무 — 가지 허용).
   */
  async putElement(
    input: unknown,
    opts: { payload?: Uint8Array; enqueue?: boolean; previous?: string } = {},
  ): Promise<PutResult> {
    const el = MemoryElement.parse(input);
    const payloadHash = opts.payload ? sha256(opts.payload) : null;
    const now = new Date().toISOString();
    const labels = labelClause(el.schema);
    const celestial = celestialOf(el.schema);

    const session = this.driver.session({ database: this.database });
    try {
      return await session.executeWrite(async (tx) => {
        // 1. 같은 origin 키의 기존 원소 조회
        const existing = await tx.run(
          `MATCH (e:Element { origin_key: $originKey })
           RETURN e.id AS id, e.content AS content, e.schema AS schema`,
          { originKey: originKey(el.origin) },
        );

        if (existing.records.length > 0) {
          const rec = existing.records[0]!;
          const oldId = rec.get("id") as string;
          const oldContent = rec.get("content") as string;
          if (sha256(oldContent) === sha256(el.content)) {
            return { id: oldId, created: false }; // 진짜 재유입 — no-op
          }
          // 2. 분기 감지 — 버리지 않고 새 원소 + 자동 INVALIDATES
          const hash8 = sha256(el.content).slice(0, 8);
          const derivedRecord = `${el.origin.record}#h${hash8}`;
          const dup = await tx.run(
            `MATCH (e:Element { origin_key: $originKey }) RETURN e.id AS id`,
            {
              originKey: originKey({ ...el.origin, record: derivedRecord }),
            },
          );
          if (dup.records.length > 0) {
            return {
              id: dup.records[0]!.get("id") as string,
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
          const oldCelestial = celestialOf(rec.get("schema") as string);
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
                "같은 origin의 내용이 달라져 분기(divergence)로 감지되었다",
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

        // 3. 신규 생성
        await this.createElementTx(tx, el, payloadHash, opts);
        return { id: el.id, created: true };
      });
    } finally {
      await session.close();
    }
  }

  /**
   * 트랜잭션 안의 원소 생성 — Payload MERGE + Element CREATE + Outbox +
   * NEXT_EPISODE 배선을 한 덩어리로. 호출자는 executeWrite 안이어야 한다.
   */
  private async createElementTx(
    tx: ManagedTransaction,
    el: MemoryElement,
    payloadHash: string | null,
    opts: { payload?: Uint8Array; enqueue?: boolean; previous?: string },
  ): Promise<void> {
    if (opts.payload && payloadHash) {
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
    // NEXT_EPISODE — Episode 라벨끼리만 (격자). 나무: 가지 허용.
    if (celestialOf(el.schema) === "Episode") {
      if (opts.previous) {
        // ① 명시적 부모 record (에이전트 세션 로그처럼 부모를 아는 소스)
        await tx.run(
          `MATCH (e:Element { id: $id })
           MATCH (p:Element:Episode)
           WHERE p.origin_source = $source AND p.origin_session = $session
             AND p.origin_record = $previous
           MERGE (p)-[l:NEXT_EPISODE]->(e)
           ON CREATE SET l += { id: $linkId, idem_key: $idemKey,
             content: '부모 record가 명시된 다음 에피소드다', weight: 1.0 }`,
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
        // ② 폴백: 같은 (source, session, schema)에서 사건 시각이 직전인 것
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
             content: '같은 세션에서 바로 다음에 일어난 에피소드다',
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

  /**
   * 링크 쓰기 — 멱등 키 (from, to, role, content 해시)로 MERGE, ON CREATE만
   * (덮어쓰기 없음). 추출 파이프라인 재실행 시 같은 링크는 no-op.
   * role은 Neo4j 관계 실타입으로 물질화된다 (graphiti 방식) —
   * 보간은 zod enum 검증을 통과한 값만 가능하므로 안전하다.
   * 격자(LINK_LATTICE) 밖의 (출발 라벨, 엣지, 도착 라벨) 조합은 거부된다.
   */
  async putLink(input: unknown): Promise<MemoryLink> {
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
        return { ...link, id: rows[0]!["id"] as string };
      });
    } finally {
      await session.close();
    }
  }

  /** 트랜잭션 안의 링크 MERGE — 격자 검사 포함. 반환 없음 = 거부. */
  private async mergeLinkTx(
    tx: ManagedTransaction,
    link: MemoryLink,
  ): Promise<Record<string, unknown>[]> {
    const lattice = LINK_LATTICE[link.role];
    const res = await tx.run(
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
    return res.records.map((rec) => Object.fromEntries(
      rec.keys.map((k) => [k as string, rec.get(k)]),
    ));
  }

  /** 원소의 Neo4j 라벨 목록 (:Element + 천체 라벨 이중 물질화 확인용) */
  async labelsOf(id: string): Promise<string[]> {
    const rows = await this.run(
      `MATCH (e:Element { id: $id }) RETURN labels(e) AS labels`,
      { id },
    );
    return rows.length ? (rows[0]!["labels"] as string[]) : [];
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
        from: r["from"],
        to: r["to"],
        role: r["role"],
        content: p["content"],
        weight: p["weight"],
      }); // idem_key는 저장 전용 메타 — 계약으로 올리지 않는다
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
                MATCH (inv:Element)-[:INVALIDATES]->(e)
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
       CALL () { MATCH (:Element)-[l]->(:Element) RETURN count(l) AS links }
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
