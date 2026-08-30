import { afterEach, beforeEach, describe, expect, test } from "bun:test";
import { mkdtempSync, rmSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { v7 as uuidv7 } from "uuid";
import { Engine } from "./engine.ts";

let root: string;
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

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "anamnesis-engine-"));
  engine = new Engine({ root });
});

afterEach(() => {
  engine.close();
  rmSync(root, { recursive: true, force: true });
});

describe("Engine — 두 DB 왕복", () => {
  test("디렉토리 배치가 계약대로 생성된다", () => {
    expect(existsSync(join(root, "vault", "vault.db"))).toBe(true);
    expect(existsSync(join(root, "memory", "memory.db"))).toBe(true);
  });

  test("remember → digest → recall 왕복", () => {
    engine.remember(msg("m1", "이노는 다크 모드를 선호한다", "2026-08-21T14:00:00+09:00"));
    engine.remember(msg("m2", "점심으로 김치찌개를 먹었다", "2026-08-21T12:00:00+09:00"));
    expect(engine.status().pendingOutbox).toBe(2);

    expect(engine.digest()).toBe(2);
    expect(engine.status()).toMatchObject({
      vaultRecords: 2,
      pendingOutbox: 0,
      elements: 2,
    });

    const hits = engine.recall("다크 모드");
    expect(hits.length).toBe(1);
    expect(hits[0]!.element.content).toContain("다크 모드");
    expect(hits[0]!.element.schema).toBe("anamnesis.original-message/1");
  });

  test("digest는 멱등 — 재실행해도 중복 원소 없음", () => {
    engine.remember(msg("m1", "하나", "2026-08-21T14:00:00+09:00"));
    engine.digest();
    engine.digest();
    engine.vault.resetOutbox();
    engine.digest(); // 전량 재소화해도 id 결정론이라 IGNORE
    expect(engine.status().elements).toBe(1);
  });

  test("snapshot(T) — 절단 시점 이후 기억은 안 보인다", () => {
    engine.remember(msg("m1", "옛날 기억이다", "2026-01-01T10:00:00+09:00"));
    engine.remember(msg("m2", "최근 기억이다", "2026-08-01T10:00:00+09:00"));
    engine.digest();

    const then = engine.recall("기억이다", { at: "2026-03-01T00:00:00Z" });
    expect(then.length).toBe(1);
    expect(then[0]!.element.content).toBe("옛날 기억이다");

    const now = engine.recall("기억이다");
    expect(now.length).toBe(2);
  });

  test("invalidates — 무효화된 사실은 기본 회상에서 빠지고, 무효화 이전 시점에선 살아있다", () => {
    engine.remember(msg("m1", "이노는 커피를 끊었다", "2026-03-01T09:00:00+09:00"));
    engine.digest();
    const fact = engine.recall("커피")[0]!.element;

    // 무효화 사건 (2026-08-15) + invalidates 링크
    const inv = engine.store.putElement({
      id: uuidv7(),
      schema: "anamnesis.invalidation/1",
      time: { value: "2026-08-15T10:00:00+09:00", precision: "second" },
      content: "이노가 커피를 다시 마시기 시작했다",
      origin: { source: "agent", session: "s", actor: "extractor", record: "inv-1" },
    });
    engine.store.putLink({
      id: uuidv7(),
      from: inv.id,
      to: fact.id,
      role: "invalidates",
      content: "커피를 다시 마시므로 끊었다는 사실은 더 이상 유효하지 않다",
    });

    // 지금 시점: 무효화됨
    expect(engine.recall("커피를 끊었다").length).toBe(0);
    // 무효화 이전 시점: 아직 참
    const before = engine.recall("커피를 끊었다", { at: "2026-05-01T00:00:00Z" });
    expect(before.length).toBe(1);
  });

  test("rebuild — memory 전량 소거 후 금고에서 동일하게 재구축", () => {
    engine.remember(msg("m1", "재구축 테스트 기억", "2026-08-21T14:00:00+09:00"));
    engine.digest();
    const before = engine.recall("재구축")[0]!.element;

    const processed = engine.rebuild();
    expect(processed).toBe(1);

    const after = engine.recall("재구축")[0]!.element;
    expect(after).toEqual(before); // id까지 결정론적으로 동일
  });
});
