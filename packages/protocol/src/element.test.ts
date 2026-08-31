import { describe, expect, test } from "bun:test";
import {
  Celestial,
  ClaimSubKind,
  KNOWN_SCHEMAS,
  MemoryElement,
  MemoryLink,
  SCHEMA_LABELS,
  TimePrecision,
} from "./index.ts";
import { LINK_LATTICE, LinkRole } from "./link.ts";

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
  test("pins the schema registry and closed vocabularies", () => {
    expect(TimePrecision.options).toEqual([
      "second",
      "minute",
      "day",
      "month",
      "year",
    ]);
    expect(Celestial.options).toEqual([
      "Episode",
      "Entity",
      "Fact",
      "Community",
    ]);
    expect(ClaimSubKind.options).toEqual([
      "fact",
      "state",
      "event",
      "preference",
      "procedure",
      "decision",
      "summary",
    ]);
    expect(KNOWN_SCHEMAS).toEqual([
      "anamnesis.original-message/1",
      "anamnesis.original-document/1",
      "anamnesis.entity/1",
      "anamnesis.claim/1",
      "anamnesis.mapping/1",
      "anamnesis.synthesis/1",
      "anamnesis.community/1",
    ]);
    expect(SCHEMA_LABELS).toEqual({
      "anamnesis.original-message/1": "Episode",
      "anamnesis.original-document/1": "Episode",
      "anamnesis.entity/1": "Entity",
      "anamnesis.claim/1": "Fact",
      "anamnesis.mapping/1": "Fact",
      "anamnesis.synthesis/1": "Fact",
      "anamnesis.community/1": "Community",
    });
  });

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
      "xanamnesis.claim/1",
      "anamnesis.claim/1-suffix",
      "anamnesis.claim/1a",
    ]) {
      const result = MemoryElement.safeParse({ ...validElement, schema });
      expect(result.success).toBe(false);
      if (!result.success) {
        expect(result.error.issues[0]!.message).toBe(
          "expected `anamnesis.<kind>/<version>` (e.g. anamnesis.claim/1)",
        );
      }
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

  test("validates every claim sub_kind and reports the exact issue", () => {
    for (const subKind of ClaimSubKind.options) {
      expect(
        MemoryElement.safeParse({
          ...validElement,
          properties: { sub_kind: subKind },
        }).success,
      ).toBe(true);
    }

    const invalid = MemoryElement.safeParse({
      ...validElement,
      properties: { sub_kind: "opinion" },
    });
    expect(invalid.success).toBe(false);
    if (!invalid.success) {
      expect(invalid.error.issues).toContainEqual({
        code: "custom",
        path: ["properties", "sub_kind"],
        message: "expected a recognized claim sub_kind",
      });
    }

    expect(
      MemoryElement.safeParse({
        ...validElement,
        schema: "anamnesis.entity/1",
        properties: { sub_kind: "opinion" },
      }).success,
    ).toBe(true);
    expect(
      MemoryElement.safeParse({
        ...validElement,
        properties: {},
      }).success,
    ).toBe(true);
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
  test("pins the complete role vocabulary and lattice", () => {
    expect(LinkRole.options).toEqual([
      "MENTIONS",
      "RELATES_TO",
      "NEXT_EPISODE",
      "HAS_MEMBER",
      "DERIVED_FROM",
      "INVALIDATES",
      "CONTRASTS",
    ]);
    expect(LINK_LATTICE).toEqual({
      NEXT_EPISODE: { from: ["Episode"], to: ["Episode"] },
      MENTIONS: { from: ["Episode", "Fact"], to: ["Entity"] },
      RELATES_TO: { from: ["Fact", "Entity"], to: ["Fact", "Entity"] },
      HAS_MEMBER: { from: ["Community"], to: ["Entity", "Fact"] },
      DERIVED_FROM: { from: ["Fact"], to: ["Episode", "Fact"] },
      INVALIDATES: {
        from: ["Fact", "Episode"],
        to: ["Fact", "Episode"],
      },
      CONTRASTS: { from: ["Fact"], to: ["Fact"] },
    });
  });

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

  test("rejects self-links with the contract message", () => {
    const result = MemoryLink.safeParse({ ...validLink, to: validLink.from });
    expect(result.success).toBe(false);
    if (!result.success) {
      expect(result.error.issues[0]!.message).toBe("self-link is not allowed");
    }
  });

  test("rejects non-positive weights", () => {
    expect(() => MemoryLink.parse({ ...validLink, weight: 0 })).toThrow();
    expect(() => MemoryLink.parse({ ...validLink, weight: -1 })).toThrow();
  });
});
