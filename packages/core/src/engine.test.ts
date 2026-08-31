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
        role: "DERIVED_FROM",
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

  test("NEXT_EPISODE — 같은 세션의 에피소드가 사건 시각 순으로 사슬 연결", async () => {
    const chain = (record: string, content: string, value: string) => ({
      time: { value, precision: "second" },
      content,
      origin: {
        source: "kakao-export",
        session: "체인방/2026-08",
        actor: "이노",
        record,
      },
    }) as const;
    const c1 = await engine.remember(chain("c1", "체인 첫째 메시지", "2026-08-23T10:00:00+09:00"));
    const c2 = await engine.remember(chain("c2", "체인 둘째 메시지", "2026-08-23T10:05:00+09:00"));
    const c3 = await engine.remember(chain("c3", "체인 셋째 메시지", "2026-08-23T10:10:00+09:00"));

    const around2 = await engine.store.linksOf(c2.id, "NEXT_EPISODE");
    expect(around2).toHaveLength(2);
    expect(around2.some((l) => l.from === c1.id && l.to === c2.id)).toBe(true);
    expect(around2.some((l) => l.from === c2.id && l.to === c3.id)).toBe(true);
    // 체인 끝단은 한 쪽만
    expect(await engine.store.linksOf(c1.id, "NEXT_EPISODE")).toHaveLength(1);
  });

  test("remember는 origin 멱등 — 같은 내용 재유입은 no-op", async () => {
    const first = await engine.remember(
      msg("dup-1", "원본 내용", "2026-08-22T10:00:00+09:00"),
    );
    const again = await engine.remember(
      msg("dup-1", "원본 내용", "2026-08-22T10:00:00+09:00"),
    );
    expect(first.created).toBe(true);
    expect(again.created).toBe(false);
    expect(again.id).toBe(first.id);
  });

  test("2단 멱등 — 같은 origin, 다른 내용은 분기: 새 Episode + 자동 INVALIDATES", async () => {
    const first = await engine.remember(
      msg("div-1", "원래 보낸 내용", "2026-08-22T10:00:00+09:00"),
    );
    const edited = await engine.remember(
      msg("div-1", "수정된 내용", "2026-08-22T10:00:00+09:00"),
    );
    expect(edited.created).toBe(true);
    expect(edited.diverged).toBe(true);
    expect(edited.invalidated).toBe(first.id);

    // 새 에피소드: record 접미 + 사건 시각은 원래 보낸 시각 유지
    const fresh = await engine.store.getElement(edited.id);
    expect(fresh!.content).toBe("수정된 내용");
    expect(fresh!.origin.record).toMatch(/^div-1#h[0-9a-f]{8}$/);
    expect(fresh!.time.value).toBe("2026-08-22T10:00:00+09:00");
    expect(fresh!.properties["diverged_at"]).toBeString();

    // (새것)-[:INVALIDATES]->(원본) 자동 배선 — 원본은 무효화됐지만 보존
    const invLinks = await engine.store.linksOf(first.id, "INVALIDATES");
    expect(invLinks.some((l) => l.from === edited.id && l.to === first.id)).toBe(true);
    const old = await engine.store.getElement(first.id);
    expect(old!.content).toBe("원래 보낸 내용"); // 덮어쓰기 없음
    expect(await engine.store.isValidAt(first.id, new Date().toISOString())).toBe(false);

    // 분기된 내용의 재유입도 멱등 — 세 번째 원소가 생기지 않는다
    const again = await engine.remember(
      msg("div-1", "수정된 내용", "2026-08-22T10:00:00+09:00"),
    );
    expect(again.created).toBe(false);
    expect(again.id).toBe(edited.id);
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

describe("Engine on Neo4j — 정본 구조 (라벨·격자·멱등)", () => {
  test("천체 라벨 이중 물질화 — :Element + :Episode/:Fact", async () => {
    const ep = await engine.remember(
      msg("lbl-1", "라벨 확인용 메시지", "2026-08-24T10:00:00+09:00"),
    );
    const claim = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T10:00:00+09:00", precision: "second" },
      content: "라벨 확인용 주장",
      origin: { source: "agent", session: "s", actor: "extractor", record: "lbl-c1" },
    });
    expect(await engine.store.labelsOf(ep.id)).toEqual(
      expect.arrayContaining(["Element", "Episode"]),
    );
    expect(await engine.store.labelsOf(claim.id)).toEqual(
      expect.arrayContaining(["Element", "Fact"]),
    );
  });

  test("격자 — 허용 쌍은 생성, 위반 쌍은 거부", async () => {
    const ep = await engine.remember(
      msg("lat-1", "격자 확인용 메시지", "2026-08-24T11:00:00+09:00"),
    );
    const claim = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T11:00:00+09:00", precision: "second" },
      content: "격자 확인용 주장",
      origin: { source: "agent", session: "s", actor: "extractor", record: "lat-c1" },
    });
    // 허용: Fact --DERIVED_FROM--> Episode
    await engine.link({
      id: uuidv7(),
      from: claim.id,
      to: ep.id,
      role: "DERIVED_FROM",
      content: "이 주장은 해당 메시지에서 추출되었다",
    });
    // 위반: Episode --RELATES_TO--> Episode (격자: Fact|Entity만)
    await expect(
      engine.link({
        id: uuidv7(),
        from: ep.id,
        to: ep.id,
        role: "RELATES_TO",
        content: "격자 위반 시도",
      }),
    ).rejects.toThrow();
  });

  test("링크 멱등 — 같은 (from, to, role, content) 재실행은 중복 생성 없음", async () => {
    const ep = await engine.remember(
      msg("idem-1", "멱등 확인용 메시지", "2026-08-24T12:00:00+09:00"),
    );
    const claim = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-24T12:00:00+09:00", precision: "second" },
      content: "멱등 확인용 주장",
      origin: { source: "agent", session: "s", actor: "extractor", record: "idem-c1" },
    });
    const mk = () => ({
      id: uuidv7(), // 호출마다 새 id를 줘도
      from: claim.id,
      to: ep.id,
      role: "DERIVED_FROM" as const,
      content: "이 주장은 해당 메시지에서 추출되었다",
    });
    const l1 = await engine.link(mk());
    const l2 = await engine.link(mk());
    expect(l2.id).toBe(l1.id); // 멱등 키로 같은 링크 반환
    const links = await engine.store.linksOf(claim.id, "DERIVED_FROM");
    expect(links.filter((l) => l.to === ep.id)).toHaveLength(1);
  });

  test("NEXT_EPISODE — previous 명시가 폴백보다 우선 (나무 배선)", async () => {
    const chain = (record: string, value: string, previous?: string) => ({
      time: { value, precision: "second" },
      content: `나무 배선 ${record}`,
      origin: { source: "agent-log", session: "sess/1", actor: "agent", record },
      ...(previous ? { previous } : {}),
    }) as const;
    const a = await engine.remember(chain("a", "2026-08-25T10:00:00+09:00"));
    // b는 시각상 a 다음이지만, 부모를 명시하지 않으면 폴백으로 a에 붙는다
    const b = await engine.remember(chain("b", "2026-08-25T10:05:00+09:00"));
    // c는 시각상 b 다음이지만 previous=a를 명시 → a에서 가지가 뻗는다
    const c = await engine.remember(chain("c", "2026-08-25T10:10:00+09:00", "a"));

    const fromA = (await engine.store.linksOf(a.id, "NEXT_EPISODE")).filter(
      (l) => l.from === a.id,
    );
    expect(fromA.map((l) => l.to).sort()).toEqual([b.id, c.id].sort()); // 가지 2개
    expect(
      (await engine.store.linksOf(b.id, "NEXT_EPISODE")).some(
        (l) => l.from === b.id && l.to === c.id,
      ),
    ).toBe(false); // b→c 사슬은 없다
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

    // 무효화 사건 (2026-08-15) + INVALIDATES 링크 — 사건도 그냥 claim이다
    const inv = await engine.put({
      id: uuidv7(),
      schema: "anamnesis.claim/1",
      time: { value: "2026-08-15T10:00:00+09:00", precision: "second" },
      content: "이노가 커피를 다시 마시기 시작했다",
      origin: { source: "agent", session: "s", actor: "extractor", record: "inv-1" },
    });
    await engine.link({
      id: uuidv7(),
      from: inv.id,
      to: fact.id,
      role: "INVALIDATES",
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
