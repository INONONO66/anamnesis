import { Database } from "bun:sqlite";
import { mkdirSync } from "node:fs";
import { join } from "node:path";
import type { ArchiveRecord } from "./record.ts";

export interface ArchiveSession {
  readonly sha256: string;
  readonly source: string;
  readonly sessionId: string;
  readonly bytes: number;
  readonly path: string;
  readonly collectedAt: string;
  readonly sessionIds?: readonly string[];
}

export class Manifest {
  private constructor(private readonly db: Database) {}

  static open(archiveDir: string): Manifest {
    mkdirSync(archiveDir, { recursive: true });
    const db = new Database(join(archiveDir, "manifest.sqlite"));
    db.exec(`
      CREATE TABLE IF NOT EXISTS sessions (
        sha256 TEXT PRIMARY KEY, source TEXT NOT NULL, session_id TEXT NOT NULL,
        bytes INTEGER NOT NULL, record_count INTEGER NOT NULL, collected_at TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS session_paths (
        sha256 TEXT NOT NULL, path TEXT NOT NULL, PRIMARY KEY(sha256, path)
      );
      CREATE TABLE IF NOT EXISTS file_sessions (
        sha256 TEXT NOT NULL, session_id TEXT NOT NULL, PRIMARY KEY(sha256, session_id)
      );
      CREATE TABLE IF NOT EXISTS texts (
        text_sha256 TEXT PRIMARY KEY, text TEXT NOT NULL
      );
      CREATE TABLE IF NOT EXISTS records (
        sha256 TEXT NOT NULL, seq INTEGER NOT NULL, record_id TEXT NOT NULL,
        actor TEXT NOT NULL, role TEXT NOT NULL, occurred_at TEXT NOT NULL,
        kind TEXT NOT NULL, canonical_kind TEXT NOT NULL, revision TEXT NOT NULL,
        line INTEGER NOT NULL, text_sha256 TEXT NOT NULL,
        session_id TEXT NOT NULL, time_precision TEXT NOT NULL, reasoning TEXT,
        PRIMARY KEY(sha256, seq)
      );
    `);
    return new Manifest(db);
  }

  hasFile(sha256: string): boolean {
    return this.db.query("SELECT 1 FROM sessions WHERE sha256 = ?").get(sha256) !== null;
  }

  addPath(sha256: string, path: string): void {
    this.db.query("INSERT OR IGNORE INTO session_paths (sha256, path) VALUES (?, ?)").run(sha256, path);
  }

  insertSession(session: ArchiveSession, records: readonly ArchiveRecord[]): void {
    this.db.transaction(() => {
      if (this.hasFile(session.sha256)) {
        this.addPath(session.sha256, session.path);
        return;
      }
      this.db.query("INSERT INTO sessions VALUES (?, ?, ?, ?, ?, ?)").run(
        session.sha256, session.source, session.sessionId, session.bytes, records.length, session.collectedAt,
      );
      this.addPath(session.sha256, session.path);
      const insertFileSession = this.db.query("INSERT OR IGNORE INTO file_sessions VALUES (?, ?)");
      for (const sessionId of session.sessionIds ?? [session.sessionId]) insertFileSession.run(session.sha256, sessionId);
      const insertText = this.db.query("INSERT OR IGNORE INTO texts VALUES (?, ?)");
      const insertRecord = this.db.query(`INSERT INTO records
        (sha256, seq, record_id, actor, role, occurred_at, kind, canonical_kind,
         revision, line, text_sha256, session_id, time_precision, reasoning)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)`);
      for (const record of records) {
        const textSha256 = new Bun.CryptoHasher("sha256").update(record.text).digest("hex");
        insertText.run(textSha256, record.text);
        insertRecord.run(
          session.sha256, record.seq, record.origin.record, record.origin.actor,
          record.role, record.occurred_at, record.kind, record.canonical_kind,
          record.revision, record.file.line, textSha256, record.origin.session,
          record.time_precision, record.reasoning ?? null,
        );
      }
    })();
  }

  recordsFor(source: string, sessionId: string): ArchiveRecord[] {
    const rows = this.db.query<{
      sha256: string; seq: number; record_id: string; actor: string; role: ArchiveRecord["role"];
      occurred_at: string; kind: string; canonical_kind: ArchiveRecord["canonical_kind"];
      revision: string; line: number; text: string; path: string;
      time_precision: ArchiveRecord["time_precision"]; reasoning: string | null;
    }, [string, string]>(`
      SELECT r.*, t.text, (SELECT path FROM session_paths
        WHERE sha256 = r.sha256 ORDER BY rowid LIMIT 1) AS path
      FROM records r JOIN sessions s ON s.sha256 = r.sha256
      JOIN texts t ON t.text_sha256 = r.text_sha256
      WHERE s.source = ? AND r.session_id = ?
      ORDER BY r.occurred_at, r.sha256, r.line, r.seq
    `).all(source, sessionId);
    return rows.map((row) => ({
      origin: { source, session: sessionId, actor: row.actor, record: row.record_id },
      seq: row.seq, role: row.role, occurred_at: row.occurred_at,
      time_precision: row.time_precision, text: row.text,
      ...(row.reasoning === null ? {} : { reasoning: row.reasoning }),
      kind: row.kind, canonical_kind: row.canonical_kind, revision: row.revision,
      file: { sha256: row.sha256, path: row.path, line: row.line },
    }));
  }

  countsBySource(): { source: string; sessions: number; records: number }[] {
    return this.db.query<{ source: string; sessions: number; records: number }, []>(`
      SELECT s.source AS source, COUNT(DISTINCT f.sha256 || char(0) || f.session_id) AS sessions,
        SUM(CASE WHEN r.sha256 IS NULL THEN 0 ELSE 1 END) AS records
      FROM sessions s JOIN file_sessions f ON f.sha256 = s.sha256
      LEFT JOIN records r ON r.sha256 = f.sha256 AND r.session_id = f.session_id
      GROUP BY s.source ORDER BY s.source
    `).all();
  }

  close(): void {
    this.db.close();
  }
}
