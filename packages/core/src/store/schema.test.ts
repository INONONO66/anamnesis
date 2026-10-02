import { describe, expect, test } from "bun:test";
import { celestialOf, labelClause, toUtc } from "./schema.ts";

describe("store schema helpers", () => {
  test("labelClause narrows to the celestial label only for known schemas", () => {
    expect(labelClause("anamnesis.original-message/1")).toBe("Element:Episode");
    expect(labelClause("anamnesis.entity/1")).toBe("Element:Entity");
    expect(labelClause("anamnesis.claim/1")).toBe("Element:Fact");
    expect(labelClause("unknown.schema/9")).toBe("Element");
    expect(celestialOf("unknown.schema/9")).toBeNull();
  });

  test("toUtc rewrites an offset timestamp as the same UTC instant", () => {
    expect(toUtc("2026-03-01T09:30:00+09:00")).toBe("2026-03-01T00:30:00.000Z");
    expect(toUtc("2026-03-01T00:30:00Z")).toBe("2026-03-01T00:30:00.000Z");
  });
});
