import { expect, test } from "bun:test";
import { EpisodeLineageError, extractionBodyDigest, type EchoLineage } from "@anamnesis/protocol";
import { StorageContractError, verifyLineageRetry, verifyEpisodeLineage } from "./digest.ts";

const uuid = (n: number) => `018f5b5e-7b1e-7abc-8def-${String(n).padStart(12, "0")}`;
const lineage: EchoLineage = { episode_id: uuid(1), lineage_mode: "receipts", parent_recall_ids: [uuid(2), uuid(3)],
  context_digests: ["a".repeat(64), "b".repeat(64)], root_episode_ids: [uuid(4)], echo_depth: 1, complete: true };
const retry = { origin_role: "assistant", lineage_mode: "receipts", parent_recall_ids: [uuid(3), uuid(2)] };

test("Episode lineage properties verify the complete retained body and reject corruption", () => {
  const { episode_id, complete, ...body } = lineage;
  const digest = extractionBodyDigest(lineage);
  const props = { ...body, lineage_complete: complete, lineage_digest: digest };
  expect(verifyEpisodeLineage(episode_id, digest, props)).toEqual(lineage);
  expect(() => verifyEpisodeLineage(episode_id, digest, { ...props, context_digests: ["c".repeat(64), "b".repeat(64)] })).toThrow("lineage_mismatch");
  expect(() => verifyEpisodeLineage(episode_id, digest, { ...props, lineage_complete: false })).toThrow("lineage_mismatch");
  expect(() => verifyEpisodeLineage(episode_id, digest, {})).toThrow("lineage_unavailable");
});
test("a lineage retry must restate the retained role, mode and parent set, in any parent order", () => {
  expect(() => verifyLineageRetry(retry, "assistant", lineage)).not.toThrow();
  const conflicts: Array<[object, string | null]> = [
    [retry, "user"], [retry, null],
    [{ origin_role: "assistant", lineage_mode: "direct", parent_recall_ids: [] }, "assistant"],
    [{ ...retry, parent_recall_ids: [uuid(2)] }, "assistant"],
    [{ ...retry, parent_recall_ids: [uuid(2), uuid(5)] }, "assistant"],
  ];
  for (const [input, role] of conflicts) {
    let caught: unknown;
    try { verifyLineageRetry(input, role, lineage); } catch (error) { caught = error; }
    expect(caught).toBeInstanceOf(StorageContractError);
    expect((caught as StorageContractError).code).toBe("revision_conflict");
    expect((caught as StorageContractError).detail).toBe(uuid(1));
  }
  for (const input of [undefined, {}, { ...retry, lineage_mode: "direct" }, { ...retry, extra: 1 }]) {
    let caught: unknown;
    try { verifyLineageRetry(input, "assistant", lineage); } catch (error) { caught = error; }
    expect(caught).toBeInstanceOf(EpisodeLineageError);
    expect((caught as EpisodeLineageError).code).toBe("invalid_params");
  }
});
