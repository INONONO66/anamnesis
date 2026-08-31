import { describe, expect, test } from "bun:test";
import { MemoryElement, MemoryLink, KNOWN_SCHEMAS } from "./index.ts";

const validElement = {
  id: "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f",
  schema: "anamnesis.claim/1",
  time: { value: "2026-08-21T14:03:22+09:00", precision: "second" },
  content: "이노는 다크 모드를 선호한다.",
  origin: {
    source: "slack",
    session: "C0123/2026-08-21",
    actor: "U098765",
    record: "1724221402.000300",
  },
} as const;

describe("MemoryElement", () => {
  test("유효한 원소를 파싱하고 properties·mass 기본값을 채운다", () => {
    const el = MemoryElement.parse(validElement);
    expect(el.properties).toEqual({});
    expect(el.time.precision).toBe("second");
    expect(el.mass).toBe(0.5);
  });

  test("고유 질량은 [0, 1] 밖을 거부한다", () => {
    expect(() =>
      MemoryElement.parse({ ...validElement, mass: 1.2 }),
    ).toThrow();
    expect(() =>
      MemoryElement.parse({ ...validElement, mass: -0.1 }),
    ).toThrow();
    expect(MemoryElement.parse({ ...validElement, mass: 0.9 }).mass).toBe(0.9);
  });

  test("KNOWN_SCHEMAS 전부가 패턴을 통과한다", () => {
    for (const s of KNOWN_SCHEMAS) {
      expect(() =>
        MemoryElement.parse({ ...validElement, schema: s }),
      ).not.toThrow();
    }
  });

  test("schema 패턴 위반을 거부한다", () => {
    for (const bad of ["claim/1", "anamnesis.Claim/1", "anamnesis.claim", "anamnesis.claim/0"]) {
      expect(() =>
        MemoryElement.parse({ ...validElement, schema: bad }),
      ).toThrow();
    }
  });

  test("타임존 오프셋 없는 시간을 거부한다", () => {
    expect(() =>
      MemoryElement.parse({
        ...validElement,
        time: { value: "2026-08-21T14:03:22", precision: "second" },
      }),
    ).toThrow();
  });

  test("알 수 없는 필드를 거부한다 (strict)", () => {
    expect(() =>
      MemoryElement.parse({ ...validElement, createdAt: "2026-08-21" }),
    ).toThrow();
  });

  test("빈 content·origin 필드를 거부한다", () => {
    expect(() =>
      MemoryElement.parse({ ...validElement, content: "" }),
    ).toThrow();
    expect(() =>
      MemoryElement.parse({
        ...validElement,
        origin: { ...validElement.origin, actor: "" },
      }),
    ).toThrow();
  });
});

const validLink = {
  id: "0192f3b2-6f8c-7d4e-a032-9b5c7d3e2f10",
  from: "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f",
  to: "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e10",
  role: "DERIVED_FROM",
  content: "이 주장은 해당 메시지에서 추출되었다.",
} as const;

describe("MemoryLink", () => {
  test("유효한 링크를 파싱하고 weight 기본값 1을 채운다", () => {
    const link = MemoryLink.parse(validLink);
    expect(link.weight).toBe(1);
  });

  test("role 7종 외를 거부한다", () => {
    expect(() =>
      MemoryLink.parse({ ...validLink, role: "timeline" }),
    ).toThrow();
    expect(() =>
      MemoryLink.parse({ ...validLink, role: "provenance" }),
    ).toThrow();
  });

  test("자기 자신으로의 링크를 거부한다", () => {
    expect(() =>
      MemoryLink.parse({ ...validLink, to: validLink.from }),
    ).toThrow();
  });

  test("음수·0 weight를 거부한다", () => {
    expect(() => MemoryLink.parse({ ...validLink, weight: 0 })).toThrow();
    expect(() => MemoryLink.parse({ ...validLink, weight: -1 })).toThrow();
  });
});
