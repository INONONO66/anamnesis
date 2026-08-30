/**
 * Vault — 불변 원본 금고.
 *
 * - append-only: 이 모듈은 INSERT만 노출한다. UPDATE/DELETE 경로 자체가 없다.
 * - 멱등: (origin.source, origin.session, origin.record) unique.
 *   같은 레코드를 다시 넣으면 기존 id를 돌려준다 — 재유입이 항상 안전하다.
 * - 원본 바이트는 content-addressed(objects/sha256/..)로 보존한다.
 * - 무결성: 레코드마다 정규화 필드의 SHA-256 digest를 저장, verify()로 감사.
 * - outbox: memory 엔진이 소화할 커서. 금고 자체는 파생을 모른다.
 */
import { DatabaseSync } from "node:sqlite";
import { createHash } from "node:crypto";
import { existsSync, mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { v7 as uuidv7 } from "uuid";
import { z } from "zod";
import { Origin, TimePoint } from "@anamnesis/protocol";

export const VaultRecordInput = z
  .object({
    time: TimePoint,
    /** 정규화된 자연어 content (어댑터가 변환) */
    content: z.string().min(1),
    origin: Origin,
    /** 소스 원본 바이트 (선택). objects/에 content-addressed 보존 */
    payload: z.instanceof(Uint8Array).optional(),
  })
  .strict();
export type VaultRecordInput = z.input<typeof VaultRecordInput>;

export interface VaultRecord {
  id: string;
  time: TimePoint;
  content: string;
  origin: Origin;
  payloadHash: string | null;
  contentDigest: string;
}

export interface AppendResult {
  id: string;
  /** false면 이미 존재하던 레코드 (멱등 재유입) */
  created: boolean;
}

export interface OutboxEntry {
  seq: number;
  recordId: string;
}

export interface IntegrityIssue {
  recordId: string;
  kind: "digest-mismatch" | "missing-object" | "object-hash-mismatch";
}

const DDL = `
CREATE TABLE IF NOT EXISTS records (
  id             TEXT PRIMARY KEY,
  time_value     TEXT NOT NULL,
  time_precision TEXT NOT NULL,
  content        TEXT NOT NULL,
  origin_source  TEXT NOT NULL,
  origin_session TEXT NOT NULL,
  origin_actor   TEXT NOT NULL,
  origin_record  TEXT NOT NULL,
  payload_hash   TEXT,
  content_digest TEXT NOT NULL,
  UNIQUE (origin_source, origin_session, origin_record)
);
CREATE TABLE IF NOT EXISTS outbox (
  seq          INTEGER PRIMARY KEY AUTOINCREMENT,
  record_id    TEXT NOT NULL REFERENCES records(id),
  processed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_records_time ON records(time_value);
CREATE INDEX IF NOT EXISTS idx_outbox_pending ON outbox(seq) WHERE processed_at IS NULL;
`;

function sha256(data: Uint8Array | string): string {
  return createHash("sha256").update(data).digest("hex");
}

/** 레코드 정규화 digest — 필드 순서를 고정한 canonical JSON */
function recordDigest(r: {
  time: TimePoint;
  content: string;
  origin: Origin;
}): string {
  return sha256(
    JSON.stringify([
      r.time.value,
      r.time.precision,
      r.content,
      r.origin.source,
      r.origin.session,
      r.origin.actor,
      r.origin.record,
    ]),
  );
}

export class Vault {
  readonly dir: string;
  private readonly db: DatabaseSync;
  private readonly objectsDir: string;

  constructor(dir: string) {
    this.dir = dir;
    mkdirSync(dir, { recursive: true });
    this.objectsDir = join(dir, "objects", "sha256");
    this.db = new DatabaseSync(join(dir, "vault.db"));
    this.db.exec("PRAGMA journal_mode = WAL");
    this.db.exec("PRAGMA foreign_keys = ON");
    this.db.exec(DDL);
  }

  /** 멱등 append. 유일한 쓰기 경로. */
  append(input: VaultRecordInput): AppendResult {
    const rec = VaultRecordInput.parse(input);

    const payloadHash = rec.payload ? this.putObject(rec.payload) : null;
    const digest = recordDigest(rec);
    const id = uuidv7();

    const inserted = this.db
      .prepare(
        `INSERT INTO records (id, time_value, time_precision, content,
           origin_source, origin_session, origin_actor, origin_record,
           payload_hash, content_digest)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (origin_source, origin_session, origin_record) DO NOTHING`,
      )
      .run(
        id,
        rec.time.value,
        rec.time.precision,
        rec.content,
        rec.origin.source,
        rec.origin.session,
        rec.origin.actor,
        rec.origin.record,
        payloadHash,
        digest,
      );

    if (Number(inserted.changes) === 0) {
      const existing = this.db
        .prepare(
          `SELECT id FROM records
           WHERE origin_source = ? AND origin_session = ? AND origin_record = ?`,
        )
        .get(rec.origin.source, rec.origin.session, rec.origin.record) as {
        id: string;
      };
      return { id: existing.id, created: false };
    }

    this.db.prepare(`INSERT INTO outbox (record_id) VALUES (?)`).run(id);
    return { id, created: true };
  }

  get(id: string): VaultRecord | null {
    const row = this.db
      .prepare(`SELECT * FROM records WHERE id = ?`)
      .get(id) as RecordRow | undefined;
    return row ? toRecord(row) : null;
  }

  /** 원본 바이트 조회 (payload를 보존한 레코드) */
  getPayload(payloadHash: string): Uint8Array | null {
    const path = this.objectPath(payloadHash);
    return existsSync(path) ? new Uint8Array(readFileSync(path)) : null;
  }

  /** outbox 미처리 항목 (memory 엔진의 소화 커서) */
  pending(limit = 100): OutboxEntry[] {
    return (
      this.db
        .prepare(
          `SELECT seq, record_id FROM outbox
           WHERE processed_at IS NULL ORDER BY seq LIMIT ?`,
        )
        .all(limit) as { seq: number; record_id: string }[]
    ).map((r) => ({ seq: r.seq, recordId: r.record_id }));
  }

  markProcessed(seqs: number[]): void {
    const stmt = this.db.prepare(
      `UPDATE outbox SET processed_at = ? WHERE seq = ?`,
    );
    const now = new Date().toISOString();
    this.db.exec("BEGIN");
    try {
      for (const seq of seqs) stmt.run(now, seq);
      this.db.exec("COMMIT");
    } catch (e) {
      this.db.exec("ROLLBACK");
      throw e;
    }
  }

  /** memory 전체 재구축용 — 커서 리셋 후 전량 재소화 */
  resetOutbox(): void {
    this.db.exec(
      `DELETE FROM outbox;
       INSERT INTO outbox (record_id) SELECT id FROM records ORDER BY id;`,
    );
  }

  /** 전수 무결성 감사 */
  verify(): IntegrityIssue[] {
    const issues: IntegrityIssue[] = [];
    const rows = this.db
      .prepare(`SELECT * FROM records`)
      .all() as unknown as RecordRow[];
    for (const row of rows) {
      const rec = toRecord(row);
      if (recordDigest(rec) !== row.content_digest) {
        issues.push({ recordId: row.id, kind: "digest-mismatch" });
      }
      if (row.payload_hash) {
        const payload = this.getPayload(row.payload_hash);
        if (!payload) {
          issues.push({ recordId: row.id, kind: "missing-object" });
        } else if (sha256(payload) !== row.payload_hash) {
          issues.push({ recordId: row.id, kind: "object-hash-mismatch" });
        }
      }
    }
    return issues;
  }

  count(): number {
    const r = this.db
      .prepare(`SELECT count(*) AS n FROM records`)
      .get() as { n: number };
    return r.n;
  }

  close(): void {
    this.db.close();
  }

  private putObject(payload: Uint8Array): string {
    const hash = sha256(payload);
    const path = this.objectPath(hash);
    if (!existsSync(path)) {
      mkdirSync(join(this.objectsDir, hash.slice(0, 2)), { recursive: true });
      writeFileSync(path, payload);
    }
    return hash;
  }

  private objectPath(hash: string): string {
    return join(this.objectsDir, hash.slice(0, 2), hash.slice(2));
  }
}

interface RecordRow {
  id: string;
  time_value: string;
  time_precision: string;
  content: string;
  origin_source: string;
  origin_session: string;
  origin_actor: string;
  origin_record: string;
  payload_hash: string | null;
  content_digest: string;
}

function toRecord(row: RecordRow): VaultRecord {
  return {
    id: row.id,
    time: TimePoint.parse({
      value: row.time_value,
      precision: row.time_precision,
    }),
    content: row.content,
    origin: {
      source: row.origin_source,
      session: row.origin_session,
      actor: row.origin_actor,
      record: row.origin_record,
    },
    payloadHash: row.payload_hash,
    contentDigest: row.content_digest,
  };
}
