import { expect, test } from "bun:test";
import { ZodError } from "zod";
import { HistoricalElement, historicalEligibility } from "./legacy-format.ts";

const origin = { source: "chat", session: "s-1", actor: "user", record: "r-1" };
const time = { value: "2026-09-01T00:00:00Z", precision: "second" };
const claim = (extra: object) => HistoricalElement.parse({ schema: "anamnesis.claim/1", content: "x", origin, mass: 0.5, properties: {}, ...extra });

test("eligibility names the missing time and the unrecognized sub_kind, and nothing else", () => {
  expect(historicalEligibility(claim({ time }))).toEqual([]);
  expect(historicalEligibility(claim({}))).toEqual(["missing-time"]);
  expect(historicalEligibility(claim({ time, properties: { sub_kind: "vibe" } }))).toEqual(["invalid-sub-kind"]);
  expect(historicalEligibility(claim({ properties: { sub_kind: "vibe" } }))).toEqual(["missing-time", "invalid-sub-kind"]);
  expect(historicalEligibility(claim({ schema: "anamnesis.entity/1", properties: { sub_kind: "vibe" } }))).toEqual([]);
});

test("a structurally broken element is a decoding error, never an eligibility reason", () => {
  for (const broken of [{ content: "" }, { origin: { ...origin, source: "" } }, { properties: { nested: Number.NaN } }]) {
    expect(() => historicalEligibility({ ...claim({ time }), ...broken })).toThrow(ZodError);
  }
});
