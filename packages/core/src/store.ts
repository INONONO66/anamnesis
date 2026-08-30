/**
 * MemoryStore — memory.db (elements / links / scores / FTS5).
 *
 * 전부 파생물이다. 이 파일(디렉토리)을 통째로 지워도 vault에서 재구축된다.
 * 원소는 불변 — putElement는 INSERT OR IGNORE (id 멱등).
 */
import { DatabaseSync } from "node:sqlite";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import { MemoryElement, MemoryLink, type LinkRole } from "@anamnesis/protocol";

const DDL = `
CREATE TABLE IF NOT EXISTS elements (
  id             TEXT PRIMARY KEY,
  schema         TEXT NOT NULL,
  time_value     TEXT NOT NULL,
  time_utc       TEXT NOT NULL,   -- UTC 정규화 (정렬·절단용)
  time_precision TEXT NOT NULL,
  content        TEXT NOT NULL,
  origin_source  TEXT NOT NULL,
  origin_session TEXT NOT NULL,
  origin_actor   TEXT NOT NULL,
  origin_record  TEXT NOT NULL,
  mass           REAL NOT NULL,
  properties     TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_elements_time    ON elements(time_utc);
CREATE INDEX IF NOT EXISTS idx_elements_schema  ON elements(schema);
CREATE INDEX IF NOT EXISTS idx_elements_session ON elements(origin_source, origin_session);

CREATE TABLE IF NOT EXISTS links (
  id      TEXT PRIMARY KEY,
  from_id TEXT NOT NULL,
  to_id   TEXT NOT NULL,
  role    TEXT NOT NULL CHECK (role IN ('provenance','about','invalidates','semantic')),
  content TEXT NOT NULL,
  weight  REAL NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_links_from ON links(from_id);
CREATE INDEX IF NOT EXISTS idx_links_to   ON links(to_id);

CREATE TABLE IF NOT EXISTS scores (
  element_id  TEXT PRIMARY KEY REFERENCES elements(id),
  mass        REAL NOT NULL,
  computed_at TEXT NOT NULL
);

CREATE VIRTUAL TABLE IF NOT EXISTS elements_fts USING fts5(
  content, content='elements', content_rowid='rowid'
);
CREATE TRIGGER IF NOT EXISTS elements_fts_insert
AFTER INSERT ON elements BEGIN
  INSERT INTO elements_fts(rowid, content) VALUES (new.rowid, new.content);
END;
`;

export interface SearchHit {
  element: MemoryElement;
  /** bm25 — 낮을수록 관련도 높음 */
  rank: number;
}

export interface StoredLink extends MemoryLink {}

/**
 * FTS5 질의 문법 주입 방지 — 토큰을 전부 따옴표로 감싸고 prefix(*)로 보낸다.
 * 한국어 조사는 접미사라("커피를") prefix 질의로 어간("커피") 매칭이 된다.
 */
function ftsQuery(raw: string): string {
  return raw
    .split(/\s+/)
    .filter(Boolean)
    .map((t) => `"${t.replaceAll('"', "")}"*`)
    .join(" ");
}

function toUtc(isoWithOffset: string): string {
  return new Date(isoWithOffset).toISOString();
}

export class MemoryStore {
  readonly dir: string;
  private readonly db: DatabaseSync;

  constructor(dir: string) {
    this.dir = dir;
    mkdirSync(dir, { recursive: true });
    this.db = new DatabaseSync(join(dir, "memory.db"));
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec("PRAGMA foreign_keys = ON");
    this.db.exec(DDL);
  }

  /** 멱등 (id 기준). 원소는 불변 — 갱신 경로 없음. */
  putElement(input: unknown): MemoryElement {
    const el = MemoryElement.parse(input);
    this.db
      .prepare(
        `INSERT OR IGNORE INTO elements
           (id, schema, time_value, time_utc, time_precision, content,
            origin_source, origin_session, origin_actor, origin_record,
            mass, properties)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`,
      )
      .run(
        el.id,
        el.schema,
        el.time.value,
        toUtc(el.time.value),
        el.time.precision,
        el.content,
        el.origin.source,
        el.origin.session,
        el.origin.actor,
        el.origin.record,
        el.mass,
        JSON.stringify(el.properties),
      );
    return el;
  }

  putLink(input: unknown): MemoryLink {
    const link = MemoryLink.parse(input);
    this.db
      .prepare(
        `INSERT OR IGNORE INTO links (id, from_id, to_id, role, content, weight)
         VALUES (?, ?, ?, ?, ?, ?)`,
      )
      .run(link.id, link.from, link.to, link.role, link.content, link.weight);
    return link;
  }

  getElement(id: string): MemoryElement | null {
    const row = this.db
      .prepare(`SELECT * FROM elements WHERE id = ?`)
      .get(id) as ElementRow | undefined;
    return row ? toElement(row) : null;
  }

  /**
   * FTS 전문검색. until을 주면 snapshot(T) 절단 — time <= T 원소만.
   */
  searchText(
    query: string,
    opts: { limit?: number; until?: string } = {},
  ): SearchHit[] {
    const q = ftsQuery(query);
    if (!q) return [];
    const limit = opts.limit ?? 20;
    const until = opts.until ? toUtc(opts.until) : null;
    const rows = this.db
      .prepare(
        `SELECT e.*, bm25(elements_fts) AS rank
         FROM elements_fts
         JOIN elements e ON e.rowid = elements_fts.rowid
         WHERE elements_fts MATCH ?
           ${until ? "AND e.time_utc <= ?" : ""}
         ORDER BY rank LIMIT ?`,
      )
      .all(...(until ? [q, until, limit] : [q, limit])) as unknown as
      (ElementRow & { rank: number })[];
    return rows.map((r) => ({ element: toElement(r), rank: r.rank }));
  }

  /** 원소에 닿아 있는 링크 전부 (그래프 이웃) */
  linksOf(id: string, role?: LinkRole): StoredLink[] {
    const rows = (
      role
        ? this.db
            .prepare(
              `SELECT * FROM links WHERE (from_id = ? OR to_id = ?) AND role = ?`,
            )
            .all(id, id, role)
        : this.db
            .prepare(`SELECT * FROM links WHERE from_id = ? OR to_id = ?`)
            .all(id, id)
    ) as unknown as LinkRow[];
    return rows.map(toLink);
  }

  /**
   * 유효성 판정: T 시점에 이 원소를 무효화한 사건이 있는가.
   * valid(fact, T) = fact.time <= T ∧ ¬∃ inv(≤T) --invalidates--> fact
   */
  isValidAt(id: string, at: string): boolean {
    const el = this.getElement(id);
    if (!el) return false;
    const atUtc = toUtc(at);
    if (toUtc(el.time.value) > atUtc) return false;
    const inv = this.db
      .prepare(
        `SELECT count(*) AS n
         FROM links l JOIN elements inv ON inv.id = l.from_id
         WHERE l.to_id = ? AND l.role = 'invalidates' AND inv.time_utc <= ?`,
      )
      .get(id, atUtc) as { n: number };
    return inv.n === 0;
  }

  counts(): { elements: number; links: number } {
    const e = this.db
      .prepare(`SELECT count(*) AS n FROM elements`)
      .get() as { n: number };
    const l = this.db
      .prepare(`SELECT count(*) AS n FROM links`)
      .get() as { n: number };
    return { elements: e.n, links: l.n };
  }

  /** 재구축용 전체 소거 — 파생물이므로 정보 손실이 아니다 */
  wipe(): void {
    this.db.exec(
      `DELETE FROM scores; DELETE FROM links;
       DELETE FROM elements_fts; DELETE FROM elements;`,
    );
  }

  close(): void {
    this.db.close();
  }
}

interface ElementRow {
  id: string;
  schema: string;
  time_value: string;
  time_precision: string;
  content: string;
  origin_source: string;
  origin_session: string;
  origin_actor: string;
  origin_record: string;
  mass: number;
  properties: string;
}

interface LinkRow {
  id: string;
  from_id: string;
  to_id: string;
  role: string;
  content: string;
  weight: number;
}

function toElement(row: ElementRow): MemoryElement {
  return MemoryElement.parse({
    id: row.id,
    schema: row.schema,
    time: { value: row.time_value, precision: row.time_precision },
    content: row.content,
    origin: {
      source: row.origin_source,
      session: row.origin_session,
      actor: row.origin_actor,
      record: row.origin_record,
    },
    mass: row.mass,
    properties: JSON.parse(row.properties),
  });
}

function toLink(row: LinkRow): StoredLink {
  return MemoryLink.parse({
    id: row.id,
    from: row.from_id,
    to: row.to_id,
    role: row.role,
    content: row.content,
    weight: row.weight,
  });
}
