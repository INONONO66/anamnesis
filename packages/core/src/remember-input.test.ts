import { describe, expect, test } from "bun:test";
import { ClaimSubKind } from "@anamnesis/protocol";
import { RememberInput } from "./engine.ts";

const time = { value: "2026-08-21T14:03:22+09:00", precision: "second" } as const;
const origin = {
  source: "slack",
  session: "C0123/2026-08-21",
  actor: "U098765",
  record: "1724221402.000300",
} as const;

const validInput = {
  time,
  content: "Ino prefers dark mode.",
  origin,
} as const;

describe("RememberInput", () => {
  test("rejects missing event time for Episode and Fact schemas", () => {
    const { time: _time, ...withoutTime } = validInput;

    expect(
      RememberInput.safeParse({
        ...withoutTime,
        schema: "anamnesis.original-message/1",
      }).success,
    ).toBe(false);
    expect(
      RememberInput.safeParse({
        ...withoutTime,
        schema: "anamnesis.claim/1",
      }).success,
    ).toBe(false);
  });

  test("rejects an invalid claim sub_kind", () => {
    expect(
      RememberInput.safeParse({
        ...validInput,
        schema: "anamnesis.claim/1",
        properties: { sub_kind: "opinion" },
      }).success,
    ).toBe(false);
  });

  test("preserves defaults, previous, and payload", () => {
    const payload = Uint8Array.from([0, 127, 255]);
    const parsed = RememberInput.parse({
      ...validInput,
      previous: "xy",
      payload,
      payload_media_type: "text/plain",
      source_revision: "revision-1",
    });

    expect(parsed.schema).toBe("anamnesis.original-message/1");
    expect(parsed.mass).toBe(0.5);
    expect(parsed.properties).toEqual({});
    expect(parsed.previous).toBe("xy");
    expect(parsed.payload).toEqual(payload);
    expect(parsed.payload_media_type).toBe("text/plain");
    expect(parsed.source_revision).toBe("revision-1");
    expect(ClaimSubKind.safeParse("fact").success).toBe(true);
  });
});
