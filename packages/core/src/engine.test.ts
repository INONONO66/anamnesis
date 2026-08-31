import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";

const TEST_DB = {
  uri: "bolt://127.0.0.1:7688",
  user: "neo4j",
  password: "anamnesis-test",
};
const AFTER_FIXTURES = "2027-01-01T00:00:00Z";

let engine: Engine;

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

async function labelsOf(id: string): Promise<string[]> {
  const driver = neo4j.driver(
    TEST_DB.uri,
    neo4j.auth.basic(TEST_DB.user, TEST_DB.password),
  );
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

beforeAll(async () => {
  // Production intentionally has no delete path, so test isolation owns cleanup.
  const admin = neo4j.driver(
    TEST_DB.uri,
    neo4j.auth.basic(TEST_DB.user, TEST_DB.password),
  );
  await admin.executeQuery("MATCH (n) DETACH DELETE n");
  await admin.close();

  engine = new Engine(TEST_DB);
  await engine.init();
});

afterAll(async () => {
  await engine.close();
});

describe("Engine storage lifecycle", () => {
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
    });
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
    const chain = (record: string, content: string, value: string) => ({
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
    expect(around2.some((link) => link.from === c1.id && link.to === c2.id)).toBe(
      true,
    );
    expect(around2.some((link) => link.from === c2.id && link.to === c3.id)).toBe(
      true,
    );
    expect(await engine.store.linksOf(c1.id, "NEXT_EPISODE")).toHaveLength(1);
  });

  test("remember is idempotent for identical origins and content", async () => {
    const first = await engine.remember(
      msg("dup-1", "Original content", "2026-08-22T10:00:00+09:00"),
    );
    const again = await engine.remember(
      msg("dup-1", "Original content", "2026-08-22T10:00:00+09:00"),
    );
    expect(first.created).toBe(true);
    expect(again.created).toBe(false);
    expect(again.id).toBe(first.id);
  });

  test("changed content at the same origin creates an invalidating branch", async () => {
    const first = await engine.remember(
      msg("div-1", "Original sent content", "2026-08-22T10:00:00+09:00"),
    );
    const edited = await engine.remember(
      msg("div-1", "Edited content", "2026-08-22T10:00:00+09:00"),
    );
    expect(edited.created).toBe(true);
    expect(edited.diverged).toBe(true);
    expect(edited.invalidated).toBe(first.id);

    const fresh = await engine.store.getElement(edited.id);
    expect(fresh!.content).toBe("Edited content");
    expect(fresh!.origin.record).toMatch(/^div-1#h[0-9a-f]{8}$/);
    expect(fresh!.time.value).toBe("2026-08-22T10:00:00+09:00");
    expect(fresh!.properties["diverged_at"]).toBeString();

    const invalidations = await engine.store.linksOf(first.id, "INVALIDATES");
    expect(
      invalidations.some(
        (link) => link.from === edited.id && link.to === first.id,
      ),
    ).toBe(true);
    expect((await engine.store.getElement(first.id))!.content).toBe(
      "Original sent content",
    );
    expect(await engine.store.isValidAt(first.id, AFTER_FIXTURES)).toBe(false);

    const again = await engine.remember(
      msg("div-1", "Edited content", "2026-08-22T10:00:00+09:00"),
    );
    expect(again.created).toBe(false);
    expect(again.id).toBe(edited.id);
  });

  test("payload bytes survive ingestion and integrity verification", async () => {
    const bytes = new TextEncoder().encode('{"raw":"source line"}');
    const result = await engine.remember({
      ...msg("p1", "Message with payload", "2026-08-22T12:00:00+09:00"),
      payload: bytes,
    });
    const element = await engine.store.getElement(result.id);
    const payloadHash = element!.properties["payload_hash"];
    expect(payloadHash).toBeString();
    expect(await engine.store.getPayload(payloadHash as string)).toEqual(bytes);
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
    await engine.link({
      id: uuidv7(),
      from: claim.id,
      to: episode.id,
      role: "DERIVED_FROM",
      content: "The claim came from the episode",
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
    await expect(
      engine.link({
        id: uuidv7(),
        from: episode.id,
        to: otherEpisode.id,
        role: "RELATES_TO",
        content: "Invalid lattice pair",
      }),
    ).rejects.toThrow("link rejected (endpoints missing or lattice violation)");
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
    });
    const first = await engine.link(makeLink());
    const second = await engine.link(makeLink());
    expect(second.id).toBe(first.id);
    const links = await engine.store.linksOf(claim.id, "DERIVED_FROM");
    expect(links.filter((link) => link.to === episode.id)).toHaveLength(1);
  });

  test("an explicit previous record takes precedence over chronology", async () => {
    const chain = (record: string, value: string, previous?: string) => ({
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
    const a = await engine.remember(
      chain("a", "2026-08-25T10:00:00+09:00"),
    );
    const b = await engine.remember(
      chain("b", "2026-08-25T10:05:00+09:00"),
    );
    const c = await engine.remember(
      chain("c", "2026-08-25T10:10:00+09:00", "a"),
    );

    const fromA = (await engine.store.linksOf(a.id, "NEXT_EPISODE")).filter(
      (link) => link.from === a.id,
    );
    expect(fromA.map((link) => link.to).sort()).toEqual([b.id, c.id].sort());
    expect(
      (await engine.store.linksOf(b.id, "NEXT_EPISODE")).some(
        (link) => link.from === b.id && link.to === c.id,
      ),
    ).toBe(false);
  });
});

describe("Engine time-axis filtering", () => {
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
    for (let index = 0; index < 6; index += 1) {
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
    expect(hits.every((hit) => candidates.slice(6).includes(hit.element.id))).toBe(
      true,
    );
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
});
