// Offline regression tests for captured-vector retrieval fusion.
import { describe, expect, test } from "bun:test";

import {
  buildFactRows,
  makeVectorLookup,
  rankEpisodes,
  rrfFuse,
} from "./retrieval-fusion.mjs";

const instruction = (text) =>
  `Instruct: Given a question about conversation history, retrieve relevant memory facts that answer the question.\nQuery: ${text}`;

const manifest = {
  samples: [
    { id: "ep-0", content: "original zero" },
    { id: "ep-1", content: "original one" },
    { id: "ep-2", content: "original two" },
  ],
};

const receipt = {
  result: {
    episodes: [
      { episode_id: "ep-0", claims: [{ content: "fact zero" }] },
      { episode_id: "ep-1", claims: [{ content: "fact one" }] },
      { episode_id: "ep-2", claims: [{ content: "bad quote fact" }] },
    ],
  },
  issues: [{ id: "ep-2", index: 0, kind: "quote" }],
};

const vectors = makeVectorLookup({
  entries: [
    ["original zero", [1, 0]],
    ["original one", [0, 1]],
    ["original two", [0.7, 0.7]],
    ["fact zero", [1, 0]],
    ["fact one", [0, 1]],
    ["bad quote fact", [1, 0]],
    [instruction("original question"), [1, 0]],
    [instruction("English question"), [0, 1]],
  ],
});

const originals = manifest.samples.map((sample, episode) => ({ episode, text: sample.content }));
const facts = buildFactRows(manifest, receipt);
const union = [...originals, ...facts];

describe("captured-vector fusion", () => {
  test("excludes exact-quote issue claims before episode-max ranking", () => {
    expect(facts.map((row) => row.text)).toEqual(["fact zero", "fact one"]);

    const english = rankEpisodes({
      rows: facts,
      queryVector: vectors.get(instruction("English question")),
      vectors,
    });
    expect(english.map((row) => row.episode)).toEqual([1, 0]);
  });

  test("uses equal-weight RRF over distinct original and English UNION ranks", () => {
    const original = rankEpisodes({
      rows: union,
      queryVector: vectors.get(instruction("original question")),
      vectors,
    });
    const english = rankEpisodes({
      rows: union,
      queryVector: vectors.get(instruction("English question")),
      vectors,
    });
    const fused = rrfFuse(original, english);

    expect(original.map((row) => row.episode)).toEqual([0, 2, 1]);
    expect(english.map((row) => row.episode)).toEqual([1, 2, 0]);
    expect(fused.map((row) => row.episode)).toEqual([0, 1, 2]);
  });

  test("breaks cosine and RRF ties by Episode index", () => {
    const tied = rankEpisodes({
      rows: [
        { episode: 2, text: "fact zero" },
        { episode: 0, text: "fact zero" },
        { episode: 1, text: "fact zero" },
      ],
      queryVector: vectors.get(instruction("original question")),
      vectors,
    });
    expect(tied.map((row) => row.episode)).toEqual([0, 1, 2]);

    expect(
      rrfFuse(
        [{ episode: 2, rank: 1 }, { episode: 0, rank: 2 }],
        [{ episode: 0, rank: 1 }, { episode: 2, rank: 2 }],
      ).map((row) => row.episode),
    ).toEqual([0, 2]);
  });

  test("throws instead of silently ranking when a captured vector is missing", () => {
    expect(() =>
      rankEpisodes({
        rows: [{ episode: 0, text: "not captured" }],
        queryVector: [1, 0],
        vectors,
      }),
    ).toThrow('Missing vector for input text: "not captured"');
  });
});
