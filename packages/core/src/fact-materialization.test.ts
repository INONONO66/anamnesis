import { createHash } from "node:crypto";
import { describe, expect, test } from "bun:test";
import { materializeFacts, recallDerived } from "./fact-materialization.ts";

const episode = { id: "ep-1", content: "Aé🙂Z", generation: 7, policyRevision: 3 };
const attempt = {
  id: "attempt-1", episodeId: episode.id, generation: 7, policyRevision: 3,
  state: "succeeded" as const, gate: "independent_shadow_review_required" as const,
  claims: [{ text: "The user prefers dark mode", start: 0, end: 10, time: "2026-09-01T00:00:00Z" }],
};
const physical = (rows: readonly any[]) => (sourceId: string, linkId: string) => rows.find(row => row.source_id === sourceId && row.link_id === linkId);

 describe("G004 fact materialization seam", () => {
  test("materializes immutable Fact custody and refuses semantic writes before gate", () => {
    expect(materializeFacts(attempt, episode, { semanticWrites: false })).toEqual({ state: "blocked", reason: "independent_shadow_review_required", facts: [] });
    const arc = { source_id: "527b4b70f974c5305da161e499616fd9", link_id: "l-1", peer_id: episode.id, role: "DERIVED_FROM" as const, generation: 7 };
    const result = materializeFacts(attempt, episode, { semanticWrites: true, independentShadowReview: true, conductingArc: arc, conductingArcLookup: physical([arc]) });
    expect(result.state).toBe("materialized");
    expect(result.facts[0]).toMatchObject({ schema: "anamnesis.claim/1", generation: 7, policyRevision: 3, source_episode_ids: ["ep-1"], lineage: { episode_id: "ep-1", attempt_id: "attempt-1", span: [0, 10] } });
  });

  test("refuses unpersisted or caller-shaped arcs", () => {
    expect(materializeFacts(attempt, episode, { semanticWrites: true, independentShadowReview: true })).toEqual({ state: "blocked", reason: "conducting_arc_unavailable", facts: [] });
    const arc = { source_id: "other-fact", link_id: "arbitrary", peer_id: episode.id, role: "DERIVED_FROM" as const, generation: 7 };
    expect(materializeFacts(attempt, episode, { semanticWrites: true, independentShadowReview: true, conductingArc: arc, conductingArcLookup: physical([]) })).toEqual({ state: "blocked", reason: "conducting_arc_stale", facts: [] });
  });

  test("recalls only trusted arcs bound to the fact's peer", () => {
    const arc = { source_id: "527b4b70f974c5305da161e499616fd9", link_id: "l-1", peer_id: episode.id, role: "DERIVED_FROM" as const, generation: 7 };
    const fact = materializeFacts(attempt, episode, { semanticWrites: true, independentShadowReview: true, conductingArc: arc, conductingArcLookup: physical([arc]) }).facts[0]!;
    const trusted = { source_id: fact.id, link_id: "l-1", peer_id: episode.id, role: "DERIVED_FROM" as const, generation: 7 };
    expect(recallDerived([fact], [trusted], { generation: 7, policyRevision: 3, limit: 1, conductingArcLookup: physical([trusted]) })).toHaveLength(1);
    expect(recallDerived([fact], [{ ...trusted, peer_id: "unrelated" }], { generation: 7, policyRevision: 3, limit: 1, conductingArcLookup: physical([{ ...trusted, peer_id: "unrelated" }]) })).toEqual([]);
    expect(recallDerived([fact], [], { generation: 7, policyRevision: 3, limit: 1, conductingArcLookup: physical([]) })).toEqual([]);
  });

  test("requires one persisted arc per claim", () => {
    const multi = { ...attempt, claims: [...attempt.claims, { text: "The user prefers compact layout", time: "2026-09-01T00:00:01Z" }] };
    const factId = (index: number, text: string, time: string) => createHash("sha256").update(`${attempt.id}:${index}:${text}:${time}`).digest("hex").slice(0, 32);
    const ids = [factId(0, attempt.claims[0]!.text, attempt.claims[0]!.time), factId(1, multi.claims[1]!.text, multi.claims[1]!.time)];
    const arcs = ids.map((source_id, index) => ({ source_id, link_id: `l-${index + 1}`, peer_id: episode.id, role: "DERIVED_FROM" as const, generation: 7 }));
    const lookup = physical(arcs);
    expect(materializeFacts(multi, episode, { semanticWrites: true, independentShadowReview: true, conductingArcs: arcs, conductingArcLookup: lookup }).state).toBe("materialized");
    expect(materializeFacts(multi, episode, { semanticWrites: true, independentShadowReview: true, conductingArcs: [arcs[0]!], conductingArcLookup: lookup })).toEqual({ state: "blocked", reason: "conducting_arc_stale", facts: [] });
  });
});
