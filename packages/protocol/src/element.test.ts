import { describe, expect, test } from "bun:test";
import { MemoryElement, MemoryLink, KNOWN_SCHEMAS } from "./index.ts";

const validElement = {
  id: "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f",
  schema: "anamnesis.claim/1",
  time: { value: "2026-08-21T14:03:22+09:00", precision: "second" },
  content: "Ino prefers dark mode.",
  origin: {
    source: "slack",
    session: "C0123/2026-08-21",
    actor: "U098765",
    record: "1724221402.000300",
  },
} as const;

describe("MemoryElement", () => {
  test("applies properties and mass defaults", () => {
    const el = MemoryElement.parse(validElement);
    expect(el.properties).toEqual({});
    expect(el.mass).toBe(0.5);
  });

  test("rejects mass outside [0, 1]", () => {
    expect(() =>
      MemoryElement.parse({ ...validElement, mass: 1.2 }),
    ).toThrow();
    expect(() =>
      MemoryElement.parse({ ...validElement, mass: -0.1 }),
    ).toThrow();
    expect(MemoryElement.parse({ ...validElement, mass: 0.9 }).mass).toBe(0.9);
  });

  test("accepts every known schema", () => {
    for (const schema of KNOWN_SCHEMAS) {
      expect(() =>
        MemoryElement.parse({ ...validElement, schema }),
      ).not.toThrow();
    }
  });

  test("rejects schema IDs outside the required pattern", () => {
    for (const schema of [
      "claim/1",
      "anamnesis.Claim/1",
      "anamnesis.claim",
      "anamnesis.claim/0",
    ]) {
      expect(() =>
        MemoryElement.parse({ ...validElement, schema }),
      ).toThrow();
    }
  });

  test("rejects UUID versions other than v7", () => {
    expect(() =>
      MemoryElement.parse({
        ...validElement,
        id: "550e8400-e29b-41d4-a716-446655440000",
      }),
    ).toThrow();
  });

  test("rejects times without a timezone offset", () => {
    expect(() =>
      MemoryElement.parse({
        ...validElement,
        time: { value: "2026-08-21T14:03:22", precision: "second" },
      }),
    ).toThrow();
  });

  test("rejects unknown fields", () => {
    expect(() =>
      MemoryElement.parse({ ...validElement, createdAt: "2026-08-21" }),
    ).toThrow();
  });

  test("rejects empty content and origin fields", () => {
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

  test("rejects non-JSON property values", () => {
    for (const value of [() => undefined, undefined, 1n]) {
      expect(() =>
        MemoryElement.parse({
          ...validElement,
          properties: { invalid: value },
        }),
      ).toThrow();
    }
  });

  test("validates claim sub_kind values", () => {
    expect(() =>
      MemoryElement.parse({
        ...validElement,
        properties: { sub_kind: "event" },
      }),
    ).not.toThrow();
    expect(() =>
      MemoryElement.parse({
        ...validElement,
        properties: { sub_kind: "opinion" },
      }),
    ).toThrow();
  });
});

const validLink = {
  id: "0192f3b2-6f8c-7d4e-a032-9b5c7d3e2f10",
  from: "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e0f",
  to: "0192f3a1-5e7b-7c3d-9f21-8a4b6c2d1e10",
  role: "DERIVED_FROM",
  content: "This claim was extracted from the message.",
} as const;

describe("MemoryLink", () => {
  test("applies the default weight", () => {
    const link = MemoryLink.parse(validLink);
    expect(link.weight).toBe(1);
  });

  test("rejects roles outside the closed vocabulary", () => {
    expect(() =>
      MemoryLink.parse({ ...validLink, role: "timeline" }),
    ).toThrow();
    expect(() =>
      MemoryLink.parse({ ...validLink, role: "provenance" }),
    ).toThrow();
  });

  test("rejects self-links", () => {
    expect(() =>
      MemoryLink.parse({ ...validLink, to: validLink.from }),
    ).toThrow();
  });

  test("rejects non-positive weights", () => {
    expect(() => MemoryLink.parse({ ...validLink, weight: 0 })).toThrow();
    expect(() => MemoryLink.parse({ ...validLink, weight: -1 })).toThrow();
  });
});
