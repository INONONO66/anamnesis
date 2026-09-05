import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import neo4j, {
  type Driver,
  type Session,
  type SessionConfig,
} from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { createHash } from "node:crypto";
import { mkdtemp, readFile, rm } from "node:fs/promises";
import { join } from "node:path";
import { homedir, tmpdir } from "node:os";
import { Engine, envConfig, RememberInput } from "./engine.ts";
import { EpisodeJournal } from "./journal.ts";
import { luceneQuery, Store } from "./store.ts";

const TEST_DB = {
  uri: "bolt://127.0.0.1:7688",
  user: "neo4j",
  password: "anamnesis-test",
};
const AFTER_FIXTURES = "2027-01-01T00:00:00Z";

let engine: Engine;
let objectsRoot: string;

function msg(record: string, content: string, value: string) {
  return {
    time: { value, precision: "second" },
    content,
    origin: {
      source: "chat-export",
      session: "friends/2026-08",
      actor: "ino",
      record,
    },
  } as const;
}

function adminDriver(): Driver {
  return neo4j.driver(
    TEST_DB.uri,
    neo4j.auth.basic(TEST_DB.user, TEST_DB.password),
  );
}

async function runAdmin(
  cypher: string,
  parameters: Record<string, string> = {},
): Promise<void> {
  const driver = adminDriver();
  try {
    await driver.executeQuery(cypher, parameters);
  } finally {
    await driver.close();
  }
}

async function schemaObjectNames(
  command: "CONSTRAINTS" | "INDEXES",
): Promise<string[]> {
  const driver = adminDriver();
  const session = driver.session();
  try {
    const result = await session.run<{ name: string }>(
      `SHOW ${command} YIELD name RETURN name`,
    );
    return result.records.map((record) => record.get("name"));
  } finally {
    await session.close();
    await driver.close();
  }
}

async function invalidatesProps(
  from: string,
  to: string,
): Promise<Record<string, string | number | null>> {
  const driver = adminDriver();
  const session = driver.session();
  try {
    const result = await session.run<{
      props: Record<string, string | number | null>;
    }>(
      `MATCH (a:Element { id: $from })-[l:INVALIDATES]->(b:Element { id: $to })
       RETURN properties(l) AS props`,
      { from, to },
    );
    return result.records[0]?.get("props") ?? {};
  } finally {
    await session.close();
    await driver.close();
  }
}

async function nextEpisodeProps(
  from: string,
  to: string,
): Promise<Record<string, string | number | null>> {
  const driver = adminDriver();
  const session = driver.session();
  try {
    const result = await session.run<{
      props: Record<string, string | number | null>;
    }>(
      `MATCH (a:Element { id: $from })-[l:NEXT_EPISODE]->(b:Element { id: $to })
       RETURN properties(l) AS props`,
      { from, to },
    );
    return result.records[0]?.get("props") ?? {};
  } finally {
    await session.close();
    await driver.close();
  }
}

/** The store driver disables lossless integers, so sequences read back numeric. */
async function readSequence(cypher: string, id?: string): Promise<number> {
  const driver = neo4j.driver(
    TEST_DB.uri,
    neo4j.auth.basic(TEST_DB.user, TEST_DB.password),
    { disableLosslessIntegers: true },
  );
  const session = driver.session();
  try {
    const result = await session.run<{ seq: number }>(
      cypher,
      id === undefined ? {} : { id },
    );
    return result.records[0]!.get("seq");
  } finally {
    await session.close();
    await driver.close();
  }
}

async function ingestSeqOf(id: string): Promise<number> {
  return readSequence(
    "MATCH (e:Element { id: $id }) RETURN e.ingest_seq AS seq",
    id,
  );
}

async function metaIngestSeq(): Promise<number> {
  return readSequence("MATCH (m:Meta { key: 'meta' }) RETURN m.ingest_seq AS seq");
}

async function labelsOf(id: string): Promise<string[]> {
  const driver = adminDriver();
  const session = driver.session();
  try {
    const result = await session.run<{ labels: string[] }>(
      "MATCH (e:Element { id: $id }) RETURN labels(e) AS labels",
      { id },
    );
    return result.records[0]?.get("labels") ?? [];
  } finally {
    await session.close();
    await driver.close();
  }
}

describe("Engine configuration", () => {
  test("uses defaults and honors every environment override", () => {
    const keys = [
      "ANAMNESIS_NEO4J_URI",
      "ANAMNESIS_NEO4J_USER",
      "ANAMNESIS_NEO4J_PASSWORD",
      "ANAMNESIS_NEO4J_DATABASE",
      "ANAMNESIS_OBJECTS_ROOT",
    ] as const;
    const saved = keys.map((key) => process.env[key]);
    try {
      for (const key of keys) delete process.env[key];
      expect(() => envConfig()).toThrow(
        "ANAMNESIS_NEO4J_PASSWORD is required",
      );

      process.env["ANAMNESIS_NEO4J_PASSWORD"] = "only-password";
      expect(envConfig()).toEqual({
        uri: "bolt://127.0.0.1:7687",
        user: "neo4j",
        password: "only-password",
        database: "neo4j",
        objectsRoot: join(homedir(), ".anamnesis", "objects"),
      });

      process.env["ANAMNESIS_NEO4J_URI"] = "bolt://db.example:9999";
      process.env["ANAMNESIS_NEO4J_USER"] = "configured-user";
      process.env["ANAMNESIS_NEO4J_PASSWORD"] = "configured-password";
      process.env["ANAMNESIS_NEO4J_DATABASE"] = "configured-database";
      process.env["ANAMNESIS_OBJECTS_ROOT"] = "/srv/anamnesis/objects";
      expect(envConfig()).toEqual({
        uri: "bolt://db.example:9999",
        user: "configured-user",
        password: "configured-password",
        database: "configured-database",
        objectsRoot: "/srv/anamnesis/objects",
      });
    } finally {
      keys.forEach((key, index) => {
        const value = saved[index];
        if (value === undefined) delete process.env[key];
        else process.env[key] = value;
      });
    }
  });

  test("accepts a short explicit previous record", () => {
    expect(
      RememberInput.parse({
        ...msg("config-previous", "Previous fixture", "2026-08-20T00:00:00Z"),
        previous: "xy",
      }).previous,
    ).toBe("xy");
  });
});

beforeAll(async () => {
  // Production intentionally has no delete path, so test isolation owns cleanup.
  const admin = adminDriver();
  await admin.executeQuery("MATCH (n) DETACH DELETE n");
  await admin.close();

  objectsRoot = await mkdtemp(join(tmpdir(), "anamnesis-objects-"));
  engine = new Engine({ ...TEST_DB, objectsRoot });
  await engine.init();
});

afterAll(async () => {
  await engine.close();
  await rm(objectsRoot, { recursive: true });
});

describe("Engine storage lifecycle", () => {
  test("init creates the complete schema and is idempotent", async () => {
    const constraints = [
      "element_id",
      "link_idem_mentions",
      "link_idem_relates_to",
      "link_idem_next_episode",
      "link_idem_has_member",
      "link_idem_derived_from",
      "link_idem_invalidates",
      "link_idem_contrasts",
      "episode_revision",
      "episode_ingest_seq",
      "payload_hash",
      "meta_key",
    ];
    const indexes = [
      "element_time",
      "element_schema",
      "outbox_pending",
      "invalidates_seek",
      "element_content",
    ];
    for (const name of await schemaObjectNames("CONSTRAINTS")) {
      await runAdmin(`DROP CONSTRAINT \`${name}\` IF EXISTS`);
    }
    for (const name of indexes) {
      await runAdmin(`DROP INDEX \`${name}\` IF EXISTS`);
    }

    await engine.init();
    await engine.init();

    expect(await schemaObjectNames("CONSTRAINTS")).toEqual(
      expect.arrayContaining(constraints),
    );
    expect(await schemaObjectNames("INDEXES")).toEqual(
      expect.arrayContaining(indexes),
    );
  });

  test("store exposes its effective default and explicit database", async () => {
    const defaultStore = new Store(TEST_DB);
    const explicitStore = new Store({ ...TEST_DB, database: "analytics" });
    expect(defaultStore.databaseName).toBe("neo4j");
    expect(explicitStore.databaseName).toBe("analytics");
    await expect(
      explicitStore.putElement({
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        time: { value: "2026-08-21T00:00:00Z", precision: "second" },
        content: "Explicit database routing fixture",
        origin: {
          source: "database-test",
          session: "routing",
          actor: "fixture",
          record: "explicit",
        },
      }),
    ).rejects.toThrow();
    await expect(explicitStore.counts()).rejects.toThrow();
    await defaultStore.close();
    await explicitStore.close();
  });

  test("sanitizes Lucene punctuation and repeated whitespace", () => {
    expect(luceneQuery('kim*chi AND "x"')).toBe("kim chi AND x");
    expect(luceneQuery("a  \t b")).toBe("a b");
  });

  test("closes write sessions on success", async () => {
    const driver = adminDriver();
    const openSession = driver.session.bind(driver);
    let closes = 0;
    driver.session = (config?: SessionConfig): Session => {
      const session = openSession(config);
      const close = session.close.bind(session);
      session.close = async () => {
        closes += 1;
        await close();
      };
      return session;
    };
    const store = new Store(TEST_DB, driver);
    await store.putElement({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-21T00:00:00Z", precision: "second" },
      content: "Session closure fixture",
      origin: {
        source: "session-test",
        session: "closure",
        actor: "fixture",
        record: "success",
      },
    });
    expect(closes).toBe(1);
    await runAdmin(
      "MATCH (e:Element { origin_source: 'session-test' }) DETACH DELETE e",
    );
    await store.close();
  });

  test("remember, digest, and recall round trip", async () => {
    await engine.remember(
      msg("m1", "Ino prefers dark mode", "2026-08-21T14:00:00+09:00"),
    );
    await engine.remember(
      msg("m2", "Lunch was kimchi stew", "2026-08-21T12:00:00+09:00"),
    );
    expect((await engine.status()).pendingOutbox).toBe(2);

    const processed = await engine.digest(async (episode, store) => {
      const claim = await store.putElement({
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        time: episode.time,
        content: `[claim] ${episode.content}`,
        origin: {
          ...episode.origin,
          actor: "extractor",
          record: `claim:${episode.origin.record}`,
        },
      });
      await store.putLink({
        id: uuidv7(),
        from: claim.id,
        to: episode.id,
        role: "DERIVED_FROM",
        content: "This claim was extracted from the source message",
      });
    }, 1);
    expect(processed).toBe(2);
    expect(await engine.status()).toMatchObject({
      elements: 4,
      links: 2,
      pendingOutbox: 0,
    });

    const hits = await engine.recall("dark mode", { at: AFTER_FIXTURES });
    expect(hits.length).toBeGreaterThanOrEqual(1);
    expect(hits[0]!.element.content).toContain("dark mode");
  });

  test("digest skips an outbox entry whose element was removed", async () => {
    const removed = await engine.remember(
      msg("removed-1", "Removed episode", "2026-08-22T12:45:00+09:00"),
    );
    await runAdmin("MATCH (e:Element { id: $id }) DETACH DELETE e", {
      id: removed.id,
    });
    let handled = 0;

    expect(
      await engine.digest(() => {
        handled += 1;
      }),
    ).toBe(1);
    expect(handled).toBe(0);
  });

  test("requeueEpisodes uses the original-message schema by default", async () => {
    await engine.remember(
      msg("requeue-default", "Default requeue", "2026-08-22T12:50:00+09:00"),
    );
    await engine.digest(() => {});
    const requeued = await engine.requeueEpisodes();
    expect(requeued).toBeGreaterThan(0);
    expect(await engine.digest(() => {})).toBe(requeued);
  });

  test("requeueEpisodes makes processed episodes available to digest again", async () => {
    const schema = "anamnesis.requeue-test/1";
    await engine.remember({
      ...msg(
        "requeue-1",
        "Episode to process twice",
        "2026-08-22T13:00:00+09:00",
      ),
      schema,
    });
    const processedIds: string[] = [];
    const handler = (episode: { id: string }) => {
      processedIds.push(episode.id);
    };

    expect(await engine.digest(handler)).toBe(1);
    expect(await engine.requeueEpisodes(schema)).toBe(1);
    expect(await engine.digest(handler)).toBe(1);
    expect(processedIds).toHaveLength(2);
    expect(processedIds[1]).toBe(processedIds[0]);
  });

  test("NEXT_EPISODE links episodes in event-time order", async () => {
    const chain = (record: string, content: string, value: string) =>
      ({
        time: { value, precision: "second" },
        content,
        origin: {
          source: "chat-export",
          session: "chain/2026-08",
          actor: "ino",
          record,
        },
      }) as const;
    const c1 = await engine.remember(
      chain("c1", "First chain message", "2026-08-23T10:00:00+09:00"),
    );
    const c2 = await engine.remember(
      chain("c2", "Second chain message", "2026-08-23T10:05:00+09:00"),
    );
    const c3 = await engine.remember(
      chain("c3", "Third chain message", "2026-08-23T10:10:00+09:00"),
    );

    const around2 = await engine.store.linksOf(c2.id, "NEXT_EPISODE");
    expect(around2).toHaveLength(2);
    expect(
      around2.some((link) => link.from === c1.id && link.to === c2.id),
    ).toBe(true);
    expect(
      around2.some((link) => link.from === c2.id && link.to === c3.id),
    ).toBe(true);
    expect(await engine.store.linksOf(c1.id, "NEXT_EPISODE")).toHaveLength(1);
  });

  test("session topology keys by session, predecessor and successor", async () => {
    const chain = (record: string, value: string, previous?: string) =>
      ({
        time: { value, precision: "second" },
        content: `Session topology key ${record}`,
        origin: {
          source: "chat-export",
          session: "topology-key/2026-08",
          actor: "ino",
          record,
        },
        ...(previous ? { previous } : {}),
      }) as const;
    const sha256 = (parts: readonly string[]) =>
      createHash("sha256").update(JSON.stringify(parts)).digest("hex");
    const sessionKey = sha256(["chat-export", "topology-key/2026-08"]);

    const k1 = await engine.remember(chain("k1", "2026-08-27T10:00:00+09:00"));
    const k2 = await engine.remember(chain("k2", "2026-08-27T10:05:00+09:00"));
    const k3 = await engine.remember(
      chain("k3", "2026-08-27T10:10:00+09:00", "k1"),
    );

    expect((await nextEpisodeProps(k1.id, k2.id))["idem_key"]).toBe(
      sha256([sessionKey, k1.id, k2.id]),
    );
    expect((await nextEpisodeProps(k1.id, k3.id))["idem_key"]).toBe(
      sha256([sessionKey, k1.id, k3.id]),
    );
    expect((await nextEpisodeProps(k2.id, k3.id))).toEqual({});
  });

  test("origin identity preserves tuple boundaries", async () => {
    const base = {
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-23T11:00:00+09:00", precision: "second" },
      content: "Boundary-sensitive origin",
      mass: 0.5,
      properties: {},
    } as const;
    const first = await engine.put({
      ...base,
      origin: { source: "ab", session: "c", actor: "fixture", record: "d" },
    });
    const second = await engine.put({
      ...base,
      id: uuidv7(),
      origin: { source: "a", session: "bc", actor: "fixture", record: "d" },
    });
    expect(first.created).toBe(true);
    expect(second.created).toBe(true);
    expect(second.id).not.toBe(first.id);
  });

  test("remember is idempotent for an identical source revision", async () => {
    const input = {
      ...msg("dup-1", "Original content", "2026-08-22T10:00:00+09:00"),
      source_revision: "revision-1",
    };
    const first = await engine.remember(input);
    const again = await engine.remember(input);
    expect(first.created).toBe(true);
    expect(again.created).toBe(false);
    expect(again.id).toBe(first.id);
  });

  test("ingest_seq increases with ingest order and duplicates consume none", async () => {
    const session = `ingest-seq-${uuidv7()}`;
    const remember = (record: string) =>
      engine.remember({
        time: { value: "2026-08-30T10:00:00+09:00", precision: "second" },
        content: `Sequence fixture ${record}`,
        origin: {
          source: "chat-export",
          session,
          actor: "ino",
          record,
        },
      });

    const first = await remember("seq-a");
    const second = await remember("seq-b");
    const duplicate = await remember("seq-b");
    const third = await remember("seq-c");

    expect(duplicate).toEqual({ id: second.id, created: false });
    const seqs = await Promise.all(
      [first, second, third].map((result) => ingestSeqOf(result.id)),
    );
    expect(seqs[1]).toBe(seqs[0]! + 1);
    // The duplicate resolved between second and third without allocating.
    expect(seqs[2]).toBe(seqs[1]! + 1);
  });

  test("concurrent remembers all commit with distinct ingest_seq", async () => {
    const run = uuidv7();
    const records = Array.from({ length: 12 }, (_, index) => index);
    const results = await Promise.all(
      records.map((index) =>
        engine.remember({
          time: { value: "2026-08-30T11:00:00+09:00", precision: "second" },
          content: `Concurrent fixture ${index}`,
          origin: {
            source: "chat-export",
            session: `concurrent-${run}-${index}`,
            actor: "ino",
            record: `concurrent-${index}`,
          },
        }),
      ),
    );

    expect(results.every((result) => result.created)).toBe(true);
    const seqs = await Promise.all(
      results.map((result) => ingestSeqOf(result.id)),
    );
    expect(new Set(seqs).size).toBe(records.length);
    expect(await metaIngestSeq()).toBeGreaterThanOrEqual(Math.max(...seqs));
  });

  test("a revision conflict throws and leaves Meta.ingest_seq intact", async () => {
    const base = msg(
      `conflict-${uuidv7()}`,
      "Conflicting original content",
      "2026-08-30T12:00:00+09:00",
    );
    const stored = await engine.remember({
      ...base,
      source_revision: "conflict-rev",
    });
    const before = await metaIngestSeq();

    await expect(
      engine.remember({
        ...base,
        content: "Rewritten under the same source revision",
        source_revision: "conflict-rev",
      }),
    ).rejects.toThrow(/revision_conflict/);

    // The increment lives in the aborted transaction, so it consumes no number.
    expect(await metaIngestSeq()).toBe(before);
    expect(await ingestSeqOf(stored.id)).toBe(before);

    const next = await engine.remember(
      msg(
        `conflict-next-${uuidv7()}`,
        "The sequence continues without a gap",
        "2026-08-30T12:01:00+09:00",
      ),
    );
    expect(await ingestSeqOf(next.id)).toBe(before + 1);
  });

  test("journal replay relies on origin idempotency", async () => {
    const directory = await mkdtemp(join(tmpdir(), "anamnesis-replay-"));
    const journal = new EpisodeJournal(directory);
    const input = msg(
      `journal-replay-${uuidv7()}`,
      "Journal replay idempotency",
      "2026-08-22T10:01:00+09:00",
    );
    try {
      await journal.append(input);
      expect(await journal.replay(engine)).toBe(1);
      const first = await engine.remember(input);
      expect(first.created).toBe(false);

      expect(await journal.replay(engine)).toBe(1);
      const second = await engine.remember(input);
      expect(second).toEqual(first);
    } finally {
      await rm(directory, { recursive: true });
    }
  });

  test("revisions are immutable, linked, and permit A to B to A", async () => {
    const base = msg(
      "revision-chain",
      "Original sent content",
      "2026-08-22T10:00:00+09:00",
    );
    const first = await engine.remember({ ...base, source_revision: "rev-a" });
    const edited = await engine.remember({
      ...base,
      content: "Edited content",
      source_revision: "rev-b",
    });
    const reverted = await engine.remember({
      ...base,
      source_revision: "rev-c",
    });

    expect(first.created).toBe(true);
    expect(edited).toMatchObject({ created: true, invalidated: first.id });
    expect(reverted).toMatchObject({ created: true, invalidated: edited.id });
    expect(new Set([first.id, edited.id, reverted.id]).size).toBe(3);

    const driver = adminDriver();
    const session = driver.session();
    try {
      const result = await session.run<{
        sourceRevision: string;
        revisionKey: string;
        previousRevisionKey: string | null;
      }>(
        `MATCH (e:Element:Episode { origin_record: $record })
         RETURN e.source_revision AS sourceRevision,
                e.revision_key AS revisionKey,
                e.previous_revision_key AS previousRevisionKey
         ORDER BY e.ingest_seq`,
        { record: "revision-chain" },
      );
      const revisions = result.records.map((record) => record.toObject());
      expect(revisions).toHaveLength(3);
      expect(revisions.map((revision) => revision.sourceRevision)).toEqual([
        "rev-a",
        "rev-b",
        "rev-c",
      ]);
      expect(revisions[0]!.previousRevisionKey).toBeNull();
      expect(revisions[1]!.previousRevisionKey).toBe(revisions[0]!.revisionKey);
      expect(revisions[2]!.previousRevisionKey).toBe(revisions[1]!.revisionKey);
    } finally {
      await session.close();
      await driver.close();
    }

    expect(await engine.store.linksOf(first.id, "INVALIDATES")).toContainEqual(
      expect.objectContaining({ from: edited.id, to: first.id }),
    );
    expect(await engine.store.linksOf(edited.id, "INVALIDATES")).toContainEqual(
      expect.objectContaining({ from: reverted.id, to: edited.id }),
    );
  });

  test("an originals INVALIDATES link keys on from, to, and role only", async () => {
    const base = msg(
      "originals-idem",
      "Originals idem content",
      "2026-08-26T10:00:00+09:00",
    );
    const first = await engine.remember({ ...base, source_revision: "o-rev-a" });
    const second = await engine.remember({
      ...base,
      content: "Originals idem content edited",
      source_revision: "o-rev-b",
    });

    const props = await invalidatesProps(second.id, first.id);
    expect(props["idem_key"]).toBe(
      createHash("sha256")
        .update(JSON.stringify([second.id, first.id, "INVALIDATES"]))
        .digest("hex"),
    );
    expect(props).toMatchObject({
      target_id: first.id,
      effective_time_utc: "2026-08-26T01:00:00.000Z",
    });
  });

  test("a derived INVALIDATES link carries its seek fields", async () => {
    const target = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-04-01T00:00:00Z", precision: "second" },
      content: "Seek field target claim",
      origin: {
        source: "agent",
        session: "seek-fields",
        actor: "extractor",
        record: "seek-target",
      },
    });
    const source = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-09-01T00:00:00Z", precision: "second" },
      content: "Seek field source claim",
      origin: {
        source: "agent",
        session: "seek-fields",
        actor: "extractor",
        record: "seek-source",
      },
    });
    await engine.link({
      id: uuidv7(),
      from: source.id,
      to: target.id,
      role: "INVALIDATES",
      content: "The later claim invalidates the earlier one",
    });

    expect(await invalidatesProps(source.id, target.id)).toMatchObject({
      target_id: target.id,
      effective_time_utc: "2026-09-01T00:00:00.000Z",
    });

    const timeless = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.entity/1",
      content: "Seek field timeless source",
      origin: {
        source: "agent",
        session: "seek-fields",
        actor: "extractor",
        record: "seek-timeless",
      },
    });
    await engine.link({
      id: uuidv7(),
      from: timeless.id,
      to: source.id,
      role: "RELATES_TO",
      content: "An unrelated derived link keeps no seek fields",
    });
    expect(
      await invalidatesProps(timeless.id, source.id),
    ).toEqual({});
  });

  test("does not invalidate divergences outside the lattice", async () => {
    const cases = [
      {
        record: "entity-to-fact",
        before: "anamnesis.entity/1",
        after: "anamnesis.claim/1",
      },
      {
        record: "fact-to-entity",
        before: "anamnesis.claim/1",
        after: "anamnesis.entity/1",
      },
    ] as const;
    for (const fixture of cases) {
      const origin = {
        source: "divergence-test",
        session: "non-invalidating",
        actor: "fixture",
        record: fixture.record,
      } as const;
      const original = await engine.put({
        id: uuidv7(),
        schema: fixture.before,
        time: { value: "2026-08-22T11:00:00Z", precision: "second" },
        content: `Original ${fixture.record}`,
        origin,
      });
      const changed = await engine.put({
        id: uuidv7(),
        schema: fixture.after,
        time: { value: "2026-08-22T11:00:00Z", precision: "second" },
        content: `Changed ${fixture.record}`,
        origin,
      });
      expect(changed).toMatchObject({ created: true, diverged: true });
      expect(changed.invalidated).toBeUndefined();
      expect(await engine.store.linksOf(original.id, "INVALIDATES")).toEqual(
        [],
      );
    }
  });

  test("verify detects persisted element content tampering", async () => {
    const result = await engine.remember(
      msg("tamper-1", "Untampered content", "2026-08-22T11:30:00+09:00"),
    );
    await runAdmin("MATCH (e:Element { id: $id }) SET e.content = $content", {
      id: result.id,
      content: "Tampered content",
    });
    expect(await engine.verify()).toContainEqual({
      elementId: result.id,
      kind: "digest-mismatch",
    });
    await runAdmin("MATCH (e:Element { id: $id }) SET e.content = $content", {
      id: result.id,
      content: "Untampered content",
    });
    expect(await engine.verify()).not.toContainEqual({
      elementId: result.id,
      kind: "digest-mismatch",
    });
  });

  test("payload bytes are externalized before metadata is committed", async () => {
    const bytes = new TextEncoder().encode('{"raw":"source line"}');
    const result = await engine.remember({
      ...msg("p1", "Message with payload", "2026-08-22T12:00:00+09:00"),
      payload: bytes,
      payload_media_type: "application/json",
    });
    const element = await engine.store.getElement(result.id);
    const payloadHash = element!.properties["payload_hash"];
    expect(payloadHash).toBeString();
    const objectBytes = await readFile(
      join(objectsRoot, (payloadHash as string).slice(0, 2), payloadHash as string),
    );
    expect(createHash("sha256").update(objectBytes).digest("hex")).toBe(
      payloadHash as string,
    );
    expect(await engine.store.getPayload(payloadHash as string)).toEqual(bytes);

    const driver = adminDriver();
    const session = driver.session();
    try {
      const graph = await session.run<{
        hash: string;
        size: number;
        mediaType: string;
        hasBytes: boolean;
      }>(
        `MATCH (:Element:Episode { id: $id })-[:HAS_PAYLOAD]->(p:Payload)
         RETURN p.hash AS hash, p.size AS size, p.media_type AS mediaType,
                p.bytes IS NOT NULL AS hasBytes`,
        { id: result.id },
      );
      expect(graph.records[0]!.toObject()).toEqual({
        hash: payloadHash as string,
        size: bytes.byteLength,
        mediaType: "application/json",
        hasBytes: false,
      });
    } finally {
      await session.close();
      await driver.close();
    }
    expect(await engine.verify()).toEqual([]);
  });

  test("recall matches CJK content", async () => {
    await engine.remember(
      msg("cjk-1", "점심으로 김치찌개를 먹었다", "2026-08-22T12:30:00+09:00"),
    );
    const hits = await engine.recall("김치찌개", { at: AFTER_FIXTURES });
    expect(hits.some((hit) => hit.element.content.includes("김치찌개"))).toBe(
      true,
    );
  });
});

describe("Engine graph contracts", () => {
  test("materializes common and celestial labels", async () => {
    const episode = await engine.remember(
      msg("lbl-1", "Episode label fixture", "2026-08-24T10:00:00+09:00"),
    );
    const claim = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T10:00:00+09:00", precision: "second" },
      content: "Fact label fixture",
      origin: {
        source: "agent",
        session: "labels",
        actor: "extractor",
        record: "lbl-c1",
      },
    });
    expect(await labelsOf(episode.id)).toEqual(
      expect.arrayContaining(["Element", "Episode"]),
    );
    expect(await labelsOf(claim.id)).toEqual(
      expect.arrayContaining(["Element", "Fact"]),
    );
  });

  test("round-trips explicit mass and tolerates absent stored properties", async () => {
    const result = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T10:15:00+09:00", precision: "second" },
      content: "Property decoding fixture",
      origin: {
        source: "agent",
        session: "decoding",
        actor: "fixture",
        record: "decode-1",
      },
      mass: 0.73,
      properties: { pinned: true },
    });
    expect(await engine.store.getElement(result.id)).toMatchObject({
      mass: 0.73,
      properties: { pinned: true },
    });
    await runAdmin("MATCH (e:Element { id: $id }) REMOVE e.properties", {
      id: result.id,
    });
    expect((await engine.store.getElement(result.id))!.properties).toEqual({});
  });

  test("does not create episode links for a non-episode", async () => {
    const parent = await engine.remember(
      msg("claim-parent", "Episode parent", "2026-08-24T10:20:00+09:00"),
    );
    const claim = await engine.store.putElement(
      {
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        time: { value: "2026-08-24T10:21:00+09:00", precision: "second" },
        content: "Non-episode with previous option",
        origin: {
          source: "chat-export",
          session: "friends/2026-08",
          actor: "fixture",
          record: "claim-child",
        },
      },
      { previous: "claim-parent" },
    );
    expect(await engine.store.linksOf(claim.id, "NEXT_EPISODE")).toEqual([]);
    expect(await engine.store.linksOf(parent.id, "NEXT_EPISODE")).not.toContainEqual(
      expect.objectContaining({ to: claim.id }),
    );
  });

  test("unknown schemas receive only the Element label", async () => {
    const result = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.unmapped/1",
      time: { value: "2026-08-24T10:30:00+09:00", precision: "second" },
      content: "Fallback label fixture",
      origin: {
        source: "agent",
        session: "labels",
        actor: "extractor",
        record: "lbl-fallback",
      },
    });
    expect(await labelsOf(result.id)).toEqual(["Element"]);
  });

  test("accepts lattice pairs and reports lattice violations", async () => {
    const episode = await engine.remember(
      msg("lat-1", "Lattice episode", "2026-08-24T11:00:00+09:00"),
    );
    const claim = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T11:00:00+09:00", precision: "second" },
      content: "Lattice claim",
      origin: {
        source: "agent",
        session: "lattice",
        actor: "extractor",
        record: "lat-c1",
      },
    });
    const accepted = await engine.link({
      id: uuidv7(),
      from: claim.id,
      to: episode.id,
      role: "DERIVED_FROM",
      content: "The claim came from the episode",
    });
    expect(accepted).toMatchObject({
      from: claim.id,
      to: episode.id,
      role: "DERIVED_FROM",
      content: "The claim came from the episode",
      weight: 1,
    });
    await expect(
      engine.link({
        id: uuidv7(),
        from: episode.id,
        to: episode.id,
        role: "RELATES_TO",
        content: "Invalid lattice pair",
      }),
    ).rejects.toThrow("self-link is not allowed");

    const otherEpisode = await engine.remember(
      msg("lat-2", "Other lattice episode", "2026-08-24T11:01:00+09:00"),
    );
    const rejectedId = uuidv7();
    await expect(
      engine.link({
        id: rejectedId,
        from: episode.id,
        to: otherEpisode.id,
        role: "RELATES_TO",
        content: "Invalid lattice pair",
      }),
    ).rejects.toThrow(
      `link rejected (endpoints missing or lattice violation): ${episode.id} -[RELATES_TO]-> ${otherEpisode.id}`,
    );
  });

  test("link identity is stable across extraction retries", async () => {
    const episode = await engine.remember(
      msg("idem-1", "Link idempotency episode", "2026-08-24T12:00:00+09:00"),
    );
    const claim = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T12:00:00+09:00", precision: "second" },
      content: "Link idempotency claim",
      origin: {
        source: "agent",
        session: "idempotency",
        actor: "extractor",
        record: "idem-c1",
      },
    });
    const makeLink = () => ({
      id: uuidv7(),
      from: claim.id,
      to: episode.id,
      role: "DERIVED_FROM" as const,
      content: "The claim came from the episode",
      weight: 0.42,
    });
    const first = await engine.link(makeLink());
    const second = await engine.link(makeLink());
    expect(second.id).toBe(first.id);
    const links = await engine.store.linksOf(claim.id, "DERIVED_FROM");
    expect(links.filter((link) => link.to === episode.id)).toHaveLength(1);
    expect(links[0]!.weight).toBe(0.42);
    expect((await engine.store.linksOf(claim.id))[0]!.role).toBe(
      "DERIVED_FROM",
    );
  });

  test("an explicit previous record takes precedence over chronology", async () => {
    const chain = (record: string, value: string, previous?: string) =>
      ({
        time: { value, precision: "second" },
        content: `Tree link ${record}`,
        origin: {
          source: "agent-log",
          session: "tree/1",
          actor: "agent",
          record,
        },
        ...(previous ? { previous } : {}),
      }) as const;
    const a = await engine.remember(chain("a", "2026-08-25T10:00:00+09:00"));
    const b = await engine.remember(chain("b", "2026-08-25T10:05:00+09:00"));
    const c = await engine.remember(
      chain("c", "2026-08-25T10:10:00+09:00", "a"),
    );
    const d = await engine.remember(
      chain("d", "2026-08-25T10:15:00+09:00", "a"),
    );

    const fromA = (await engine.store.linksOf(a.id, "NEXT_EPISODE")).filter(
      (link) => link.from === a.id,
    );
    expect(fromA.map((link) => link.to).sort()).toEqual(
      [b.id, c.id, d.id].sort(),
    );
    expect(
      (await engine.store.linksOf(b.id, "NEXT_EPISODE")).some(
        (link) => link.from === b.id && link.to === c.id,
      ),
    ).toBe(false);
  });
});

describe("Engine time-axis filtering", () => {
  test("snapshot time filters event time and retains timeless elements", async () => {
    const timeless = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.entity/1",
      content: "Snapshot contract sentinel",
      origin: {
        source: "time-test",
        session: "episode-only",
        actor: "fixture",
        record: "timeless",
      },
    });
    const future = await engine.remember({
      ...msg(
        "future-snapshot",
        "Snapshot contract sentinel",
        "2028-01-01T00:00:00Z",
      ),
      source_revision: "future-1",
    });

    const hits = await engine.recall("Snapshot contract sentinel", {
      at: "2027-01-01T00:00:00Z",
    });
    expect(hits.some((hit) => hit.element.id === timeless.id)).toBe(true);
    expect(hits.some((hit) => hit.element.id === future.id)).toBe(false);
    expect((await engine.store.getElement(timeless.id))!.time).toBeUndefined();
  });

  test("empty search is exact and unfiltered search includes invalidated data", async () => {
    expect(await engine.store.searchText(" \\ / * ")).toEqual([]);
    const original = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-02-01T00:00:00Z", precision: "second" },
      content: "Unfiltered invalidated sentinel",
      origin: {
        source: "search-test",
        session: "defaults",
        actor: "fixture",
        record: "original",
      },
    });
    const replacement = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-02-02T00:00:00Z", precision: "second" },
      content: "Replacement sentinel",
      origin: {
        source: "search-test",
        session: "defaults",
        actor: "fixture",
        record: "replacement",
      },
    });
    await engine.link({
      id: uuidv7(),
      from: replacement.id,
      to: original.id,
      role: "INVALIDATES",
      content: "Replacement invalidates original",
    });
    expect(
      (await engine.store.searchText("Unfiltered invalidated sentinel")).some(
        (hit) => hit.element.id === original.id,
      ),
    ).toBe(true);
  });

  test("snapshot recall excludes memories after the cutoff", async () => {
    await engine.remember(
      msg("t1", "A distant archival memory", "2026-01-01T10:00:00+09:00"),
    );
    await engine.remember(
      msg("t2", "A recent archival memory", "2026-08-01T10:00:00+09:00"),
    );

    const then = await engine.recall("archival memory", {
      at: "2026-03-01T00:00:00Z",
    });
    expect(
      then.some((hit) => hit.element.content === "A distant archival memory"),
    ).toBe(true);
    expect(
      then.some((hit) => hit.element.content === "A recent archival memory"),
    ).toBe(false);

    const later = await engine.recall("archival memory", {
      at: AFTER_FIXTURES,
    });
    expect(
      later.some((hit) => hit.element.content === "A recent archival memory"),
    ).toBe(true);
  });

  test("invalidation applies only at and after its event time", async () => {
    const fact = await engine.remember(
      msg("f1", "Ino stopped drinking coffee", "2026-03-01T09:00:00+09:00"),
    );
    const invalidation = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-15T10:00:00+09:00", precision: "second" },
      content: "Ino started drinking coffee again",
      origin: {
        source: "agent",
        session: "invalidation",
        actor: "extractor",
        record: "inv-1",
      },
    });
    await engine.link({
      id: uuidv7(),
      from: invalidation.id,
      to: fact.id,
      role: "INVALIDATES",
      content: "Resuming coffee invalidates the earlier statement",
    });

    const after = await engine.recall("stopped drinking coffee", {
      at: AFTER_FIXTURES,
    });
    expect(after.some((hit) => hit.element.id === fact.id)).toBe(false);

    const before = await engine.recall("stopped drinking coffee", {
      at: "2026-05-01T00:00:00Z",
    });
    expect(before.some((hit) => hit.element.id === fact.id)).toBe(true);
    expect(await engine.store.isValidAt(fact.id, "2026-05-01T00:00:00Z")).toBe(
      true,
    );
    expect(await engine.store.isValidAt(fact.id, AFTER_FIXTURES)).toBe(false);
  });

  test("a Fact carries event time exactly like an Episode", async () => {
    const backdated = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2019-03-01T00:00:00Z", precision: "month" },
      content: "Fact event time sentinel moved to Seoul",
      origin: {
        source: "agent",
        session: "fact-time",
        actor: "extractor",
        record: "fact-time-1",
      },
    });

    expect((await engine.store.getElement(backdated.id))!.time).toEqual({
      value: "2019-03-01T00:00:00Z",
      precision: "month",
    });
    expect(
      await engine.store.isValidAt(backdated.id, "2018-01-01T00:00:00Z"),
    ).toBe(false);
    expect(
      await engine.store.isValidAt(backdated.id, "2020-01-01T00:00:00Z"),
    ).toBe(true);

    const before = await engine.recall("Fact event time sentinel", {
      at: "2018-01-01T00:00:00Z",
    });
    expect(before.some((hit) => hit.element.id === backdated.id)).toBe(false);
    const at = await engine.recall("Fact event time sentinel", {
      at: AFTER_FIXTURES,
    });
    expect(at.some((hit) => hit.element.id === backdated.id)).toBe(true);
    expect(await engine.verify()).not.toContainEqual(
      expect.objectContaining({ elementId: backdated.id }),
    );
  });

  test("a correction is retroactive through its backdated time, not a missing one", async () => {
    const fact = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-03-01T00:00:00Z", precision: "second" },
      content: "Retroactive correction sentinel claim",
      origin: {
        source: "agent",
        session: "correction",
        actor: "extractor",
        record: "corr-target",
      },
    });
    await expect(
      engine.put({
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        content: "Retroactive correction sentinel corrector",
        origin: {
          source: "agent",
          session: "correction",
          actor: "extractor",
          record: "corr-source",
        },
      }),
    ).rejects.toThrow(/event time is required for a Fact/);

    const corrector = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-03-01T00:00:00Z", precision: "second" },
      content: "Retroactive correction sentinel corrector",
      origin: {
        source: "agent",
        session: "correction",
        actor: "extractor",
        record: "corr-source",
      },
    });
    await engine.link({
      id: uuidv7(),
      from: corrector.id,
      to: fact.id,
      role: "INVALIDATES",
      content: "The record was wrong from the moment it took effect",
    });
    expect(await invalidatesProps(corrector.id, fact.id)).toMatchObject({
      target_id: fact.id,
      effective_time_utc: "2026-03-01T00:00:00.000Z",
    });

    expect(
      await engine.store.isValidAt(fact.id, "2026-04-01T00:00:00Z"),
    ).toBe(false);
    expect(await engine.store.isValidAt(fact.id, AFTER_FIXTURES)).toBe(false);
    const hits = await engine.recall("Retroactive correction sentinel claim", {
      at: AFTER_FIXTURES,
    });
    expect(hits.some((hit) => hit.element.id === fact.id)).toBe(false);
  });

  test("a backdated correction hides its target from the target's own time", async () => {
    const wrong = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-03-01T00:00:00Z", precision: "second" },
      content: "Backdated correction sentinel wrong claim",
      origin: {
        source: "agent",
        session: "backdated",
        actor: "extractor",
        record: "backdated-wrong",
      },
    });
    const corrected = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-03-01T00:00:00Z", precision: "second" },
      content: "Backdated correction sentinel right claim",
      origin: {
        source: "agent",
        session: "backdated",
        actor: "extractor",
        record: "backdated-right",
      },
    });
    await engine.link({
      id: uuidv7(),
      from: corrected.id,
      to: wrong.id,
      role: "INVALIDATES",
      content: "The record was wrong, so the correction carries its time",
    });

    expect(
      await engine.store.isValidAt(wrong.id, "2026-02-28T00:00:00Z"),
    ).toBe(false);
    expect(
      await engine.store.isValidAt(wrong.id, "2026-03-01T00:00:00Z"),
    ).toBe(false);
    expect(await engine.store.isValidAt(wrong.id, AFTER_FIXTURES)).toBe(false);
    expect(
      await engine.store.isValidAt(corrected.id, AFTER_FIXTURES),
    ).toBe(true);
  });

  test("recall applies validity filtering before its exact limit", async () => {
    const candidates: string[] = [];
    for (let index = 0; index < 8; index += 1) {
      const result = await engine.put({
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        time: { value: "2026-06-01T00:00:00Z", precision: "second" },
        content: `Exact pagination target ${index}`,
        origin: {
          source: "pagination-test",
          session: "exact-limit",
          actor: "fixture",
          record: `target-${index}`,
        },
      });
      candidates.push(result.id);
    }
    for (let index = 0; index < 4; index += 1) {
      const invalidation = await engine.put({
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        time: { value: "2026-07-01T00:00:00Z", precision: "second" },
        content: `Retraction ${index}`,
        origin: {
          source: "pagination-test",
          session: "exact-limit",
          actor: "fixture",
          record: `retraction-${index}`,
        },
      });
      await engine.link({
        id: uuidv7(),
        from: invalidation.id,
        to: candidates[index]!,
        role: "INVALIDATES",
        content: `Retracts target ${index}`,
      });
    }

    const hits = await engine.recall("Exact pagination target", {
      limit: 2,
      at: AFTER_FIXTURES,
    });
    expect(hits).toHaveLength(2);
    expect(
      hits.every((hit) => candidates.slice(4).includes(hit.element.id)),
    ).toBe(true);
  });

  test("a missing element is not valid", async () => {
    expect(await engine.store.isValidAt(uuidv7(), AFTER_FIXTURES)).toBe(false);
  });

  test("future elements are invalid before their event time", async () => {
    const result = await engine.remember(
      msg("fut1", "A future event", "2026-12-25T00:00:00+09:00"),
    );
    expect(
      await engine.store.isValidAt(result.id, "2026-11-01T00:00:00Z"),
    ).toBe(false);
    expect(
      await engine.store.isValidAt(result.id, "2026-12-31T00:00:00Z"),
    ).toBe(true);
  });

  test("close shuts down the underlying driver", async () => {
    const closed = new Engine(TEST_DB);
    await closed.init();
    await closed.close();
    await expect(closed.status()).rejects.toThrow();
  });
});
