import { afterAll, beforeAll, describe, expect, test } from "bun:test";
import neo4j from "neo4j-driver";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";

const TEST_DB = {
  uri: "bolt://127.0.0.1:7688",
  user: "neo4j",
  password: "anamnesis-test",
};

let engine: Engine;

function msg(record: string, content: string, value: string) {
  return {
    time: { value, precision: "second" },
    content,
    origin: {
      source: "kakao-export",
      session: "친구방/2026-08",
      actor: "이노",
      record,
    },
  } as const;
}

beforeAll(async () => {
  // 테스트 전 전체 초기화 — 데몬 코드에는 삭제 경로가 없으므로 테스트가 직접 지운다
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

describe("Engine on Neo4j — 기본 축적", () => {
  test("remember → digest(추출 핸들러) → recall 왕복", async () => {
    await engine.remember(
      msg("m1", "이노는 다크 모드를 선호한다", "2026-08-21T14:00:00+09:00"),
    );
    await engine.remember(
      msg("m2", "점심으로 김치찌개를 먹었다", "2026-08-21T12:00:00+09:00"),
    );
    expect((await engine.status()).pendingOutbox).toBe(2);

    // 추출 핸들러 자리 검증: 에피소드마다 claim + provenance 링크 파생
    const processed = await engine.digest(async (episode, store) => {
      const claim = await store.putElement({
        id: uuidv7(),
        schema: "anamnesis.claim/1",
        time: episode.time,
        content: `[주장] ${episode.content}`,
        origin: { ...episode.origin, actor: "extractor", record: `claim:${episode.origin.record}` },
      });
      await store.putLink({
        id: uuidv7(),
        from: claim.id,
        to: episode.id,
        role: "provenance",
        content: "이 주장은 해당 원본 메시지에서 추출되었다",
      });
    });
    expect(processed).toBe(2);
    expect(await engine.status()).toMatchObject({
      elements: 4, // 에피소드 2 + 주장 2
      links: 2,
      pendingOutbox: 0,
    });

    const hits = await engine.recall("다크 모드");
    expect(hits.length).toBeGreaterThanOrEqual(1);
    expect(hits[0]!.element.content).toContain("다크 모드");
  });

  test("remember는 origin 멱등 — 재유입해도 중복·덮어쓰기 없음", async () => {
    const first = await engine.remember(
      msg("dup-1", "원본 내용", "2026-08-22T10:00:00+09:00"),
    );
    const again = await engine.remember(
      msg("dup-1", "다른 내용으로 위조 시도", "2026-08-22T11:00:00+09:00"),
    );
    expect(first.created).toBe(true);
    expect(again.created).toBe(false);
    expect(again.id).toBe(first.id);
    const stored = await engine.store.getElement(first.id);
    expect(stored!.content).toBe("원본 내용"); // ON CREATE만 — 덮어쓰기 불가
  });

  test("payload 보존 및 verify() 무결성 감사 통과", async () => {
    const bytes = new TextEncoder().encode('{"raw":"카톡 원본 라인"}');
    const r = await engine.remember({
      ...msg("p1", "페이로드 있는 메시지", "2026-08-22T12:00:00+09:00"),
      payload: bytes,
    });
    const el = await engine.store.getElement(r.id);
    expect(el).not.toBeNull();
    const issues = await engine.verify();
    expect(issues).toEqual([]);
  });
});

describe("Engine on Neo4j — 시간축 필터링", () => {
  test("snapshot(T) — 절단 시점 이후 기억은 존재하지 않았던 것", async () => {
    await engine.remember(msg("t1", "아주 옛날 기억이다", "2026-01-01T10:00:00+09:00"));
    await engine.remember(msg("t2", "아주 최근 기억이다", "2026-08-01T10:00:00+09:00"));

    const then = await engine.recall("아주 기억이다", { at: "2026-03-01T00:00:00Z" });
    expect(then.some((h) => h.element.content === "아주 옛날 기억이다")).toBe(true);
    expect(then.some((h) => h.element.content === "아주 최근 기억이다")).toBe(false);

    const now = await engine.recall("아주 기억이다");
    expect(now.some((h) => h.element.content === "아주 최근 기억이다")).toBe(true);
  });

  test("invalidates — 무효화 이후엔 회상에서 빠지고, 이전 시점에선 살아있다", async () => {
    const fact = await engine.remember(
      msg("f1", "이노는 커피를 끊었다", "2026-03-01T09:00:00+09:00"),
    );

    // 무효화 사건 (2026-08-15) + invalidates 링크
    const inv = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.invalidation/1",
      time: { value: "2026-08-15T10:00:00+09:00", precision: "second" },
      content: "이노가 커피를 다시 마시기 시작했다",
      origin: { source: "agent", session: "s", actor: "extractor", record: "inv-1" },
    });
    await engine.link({
      id: uuidv7(),
      from: inv.id,
      to: fact.id,
      role: "invalidates",
      content: "커피를 다시 마시므로 끊었다는 사실은 더 이상 유효하지 않다",
    });

    // 지금 시점: 무효화된 사실은 회상에 안 나옴 (무효화 사건 자체는 나올 수 있음)
    const now = await engine.recall("커피를 끊었다");
    expect(now.some((h) => h.element.id === fact.id)).toBe(false);

    // 무효화 이전 시점: 아직 참
    const before = await engine.recall("커피를 끊었다", { at: "2026-05-01T00:00:00Z" });
    expect(before.some((h) => h.element.id === fact.id)).toBe(true);

    // isValidAt 직접 판정도 동일
    expect(await engine.store.isValidAt(fact.id, "2026-05-01T00:00:00Z")).toBe(true);
    expect(await engine.store.isValidAt(fact.id, new Date().toISOString())).toBe(false);
  });

  test("미래 시점 원소는 그 이전 시점에서 무효", async () => {
    const r = await engine.remember(
      msg("fut1", "미래에 기록된 무언가", "2026-12-25T00:00:00+09:00"),
    );
    expect(await engine.store.isValidAt(r.id, "2026-11-01T00:00:00Z")).toBe(false);
    expect(await engine.store.isValidAt(r.id, "2026-12-31T00:00:00Z")).toBe(true);
  });
});
