import { expect, test } from "bun:test";
import { scoreRanks } from "./recall-eval.ts";

const at = (index: number, thread: string | null = null) =>
  ({ key: { source: "example", room: "room", thread, message: String(index) }, index });

test("strict scores ignore neighbors and duplicate records, and truncate after rank ten", () => {
  const gold = [at(4), at(8)];
  const ranked = [at(2), at(4, "other"), at(4), at(4), null, null, null, null, null, null, at(8)];
  const strict = scoreRanks(gold, ranked, 0);
  expect(strict).toEqual({
    ranks: [3], recallAt1: 0, recallAt5: 1, recallAt10: 1,
    mrrAt10: 1 / 3, ndcgAt10: 0.5 / (1 + 1 / Math.log2(3)),
  });
});

test("lenient scores match within two positions and recalculate overlapping assignments without double credit", () => {
  const gold = [at(4), at(7)];
  const ranked = [at(5), at(8, "other"), at(5), at(2), at(7)];
  const lenient = scoreRanks(gold, ranked, 2);
  expect(lenient).toEqual({
    ranks: [1, 4], recallAt1: 1, recallAt5: 1, recallAt10: 1,
    mrrAt10: 1, ndcgAt10: (1 + 1 / Math.log2(5)) / (1 + 1 / Math.log2(3)),
  });
});

test("empty results are zero for every rank metric", () => {
  expect(scoreRanks([at(1)], [], 0)).toEqual({
    ranks: [], recallAt1: 0, recallAt5: 0, recallAt10: 0, mrrAt10: 0, ndcgAt10: 0,
  });
});
