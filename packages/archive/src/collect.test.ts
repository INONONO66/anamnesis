import { expect, test } from "bun:test";
import { createHash } from "node:crypto";
import { copyFile, mkdtemp, readFile, readdir, rm, unlink } from "node:fs/promises";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { Database } from "bun:sqlite";
import { collect } from "./collect.ts";
import { Manifest } from "./manifest.ts";
import { revisionOf } from "./record.ts";
import type { ArchiveRecord } from "./record.ts";
import type { SourceAdapter } from "./sources/types.ts";

const text = "first\nverbatim";
const occurred_at = "2026-10-10T12:00:00.000Z";
const sha256 = createHash("sha256").update("one").digest("hex");
const fixture: ArchiveRecord = {
  origin: { source: "fixture", session: "s", actor: "user", record: "r" },
  seq: 0, role: "user", occurred_at, time_precision: "second", text,
  kind: "message", canonical_kind: "message", revision: revisionOf(occurred_at, text),
  file: { sha256, path: "/fixture/one", line: 1 },
};

test("insertSession is idempotent and a second path only changes session_paths", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-manifest-"));
  try {
    const manifest = Manifest.open(root);
    const session = { sha256, source: "fixture", sessionId: "s", bytes: 3, path: "/fixture/one", collectedAt: occurred_at };
    manifest.insertSession(session, [fixture]);
    manifest.insertSession(session, [fixture]);
    manifest.insertSession({ ...session, path: "/fixture/two" }, [fixture]);
    expect(manifest.countsBySource()).toEqual([{ source: "fixture", sessions: 1, records: 1 }]);
    expect(manifest.recordsFor("fixture", "s")).toEqual([fixture]);
    manifest.close();
    const db = new Database(join(root, "manifest.sqlite"));
    expect(db.query("SELECT path FROM session_paths ORDER BY path").all()).toEqual([
      { path: "/fixture/one" }, { path: "/fixture/two" },
    ]);
    db.close();
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("manifest preserves provider origin separately from adapter directory", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-origin-"));
  try {
    const manifest = Manifest.open(root);
    manifest.insertSession({ sha256, source: "agentlog", sessionId: "s", bytes: 3, path: "/fixture/one", collectedAt: occurred_at },
      [{ ...fixture, origin: { ...fixture.origin, source: "codex" } }]);
    expect(manifest.recordsFor("agentlog", "s")[0]?.origin.source).toBe("codex");
    manifest.close();
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("manifest rejects old layouts before an insert", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-old-layout-"));
  try {
    const db = new Database(join(root, "manifest.sqlite"));
    db.run("CREATE TABLE records (sha256 TEXT, seq INTEGER)");
    db.close();
    expect(() => Manifest.open(root)).toThrow("manifest layout outdated; collect into a fresh --archive dir");
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("identical file bytes are collected once per adapter kind", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-sources-"));
  try {
    const input = join(root, "one");
    const archiveDir = join(root, "archive");
    await Bun.write(input, "one");
    for (const kind of ["first", "second"]) {
      const adapter: SourceAdapter = {
        kind,
        async *files() { yield input; },
        async parse() {
          const { seq, file, ...record } = fixture;
          return [{ sessionId: "s", records: [{ ...record, origin: { ...record.origin, source: `${kind}-provider` }, line: 1 }] }];
        },
      };
      expect(await collect({ adapter, root, archiveDir })).toEqual({ files: 1, sessions: 1, records: 1, skipped: 0 });
      const saved = JSON.parse(await readFile(join(archiveDir, "records", kind, "s.jsonl"), "utf8"));
      expect(saved.origin.source).toBe(`${kind}-provider`);
    }
    const manifest = Manifest.open(archiveDir);
    expect(manifest.countsBySource()).toEqual([
      { source: "first", sessions: 1, records: 1 }, { source: "second", sessions: 1, records: 1 },
    ]);
    manifest.close();
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("collect preserves JSONL and manifest across repeated paths without reparsing", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-collect-"));
  try {
    const input = join(root, "inputs");
    const archiveDir = join(root, "archive");
    await Bun.write(join(input, "one"), "one");
    let parsed = 0;
    const adapter: SourceAdapter = {
      kind: "fixture",
      async *files() {
        for (const name of (await readdir(input)).sort()) yield join(input, name);
      },
      async parse() {
        parsed++;
        const { seq, file, ...record } = fixture;
        return [{ sessionId: "s", records: [{ ...record, line: 1 }] }];
      },
    };
    expect(await collect({ adapter, root: input, archiveDir })).toEqual({ files: 1, sessions: 1, records: 1, skipped: 0 });
    const jsonl = join(archiveDir, "records", "fixture", "s.jsonl");
    const before = await readFile(jsonl);
    const manifest = Manifest.open(archiveDir);
    const counts = manifest.countsBySource();
    manifest.close();
    expect(await collect({ adapter, root: input, archiveDir })).toEqual({ files: 1, sessions: 0, records: 0, skipped: 1 });
    expect(await readFile(jsonl)).toEqual(before);
    await copyFile(join(input, "one"), join(input, "two"));
    expect(await collect({ adapter, root: input, archiveDir })).toEqual({ files: 2, sessions: 0, records: 0, skipped: 2 });
    expect(parsed).toBe(1);
    const again = Manifest.open(archiveDir);
    expect(again.countsBySource()).toEqual(counts);
    again.close();
    const db = new Database(join(archiveDir, "manifest.sqlite"));
    expect(db.query("SELECT path FROM session_paths").all()).toHaveLength(2);
    db.close();
    expect(await readFile(jsonl)).toEqual(before);
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});

test("collect restores a missing JSONL for a skipped file without reparsing", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-repair-"));
  try {
    const input = join(root, "one");
    const archiveDir = join(root, "archive");
    await Bun.write(input, "one");
    let parsed = 0;
    const adapter: SourceAdapter = {
      kind: "fixture",
      async *files() { yield input; },
      async parse() {
        parsed++;
        const { seq, file, ...record } = fixture;
        return [{ sessionId: "s", records: [{ ...record, line: 1 }] }];
      },
    };
    await collect({ adapter, root, archiveDir });
    const destination = join(archiveDir, "records", "fixture", "s.jsonl");
    const expected = await readFile(destination);
    await unlink(destination);
    expect(await collect({ adapter, root, archiveDir })).toEqual({ files: 1, sessions: 0, records: 0, skipped: 1 });
    expect(await readFile(destination)).toEqual(expected);
    expect(parsed).toBe(1);
  } finally { await rm(root, { recursive: true, force: true }); }
});

test("collect rewrites one session JSONL from every distinct source file in timestamp order", async () => {
  const root = await mkdtemp(join(tmpdir(), "archive-merge-"));
  try {
    const archiveDir = join(root, "archive");
    await Bun.write(join(root, "one"), "one");
    await Bun.write(join(root, "two"), "two");
    const adapter: SourceAdapter = {
      kind: "fixture",
      async *files() {
        yield join(root, "one");
        yield join(root, "two");
      },
      async parse(path) {
        const second = path.endsWith("two");
        const { seq, file, ...record } = fixture;
        return [{ sessionId: "s", records: [{
          ...record, line: 1,
          occurred_at: second ? "2026-10-09T12:00:00.000Z" : occurred_at,
          text: second ? "earlier" : text,
          revision: second ? revisionOf("2026-10-09T12:00:00.000Z", "earlier") : fixture.revision,
        }] }];
      },
    };
    expect(await collect({ adapter, root, archiveDir })).toEqual({ files: 2, sessions: 2, records: 2, skipped: 0 });
    const lines = (await readFile(join(archiveDir, "records", "fixture", "s.jsonl"), "utf8")).trimEnd().split("\n");
    expect(lines.map((line) => JSON.parse(line).text)).toEqual(["earlier", text]);
    const manifest = Manifest.open(archiveDir);
    expect(manifest.countsBySource()).toEqual([{ source: "fixture", sessions: 2, records: 2 }]);
    manifest.close();
  } finally {
    await rm(root, { recursive: true, force: true });
  }
});
