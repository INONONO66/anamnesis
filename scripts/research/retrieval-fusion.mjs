#!/usr/bin/env node
// Offline comparison of retrieval paths using only vectors captured by the
// English extraction pilot. This script never contacts an embedding or model
// service; a missing captured vector is a hard error.

import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { dirname } from "node:path";

export const QUERY_INSTRUCTION =
  "Instruct: Given a question about conversation history, retrieve relevant memory facts that answer the question.";
export const RRF_K = 60;
export const RRF_WEIGHT = 0.5;
export const REPORT_SCHEMA = "anamnesis.research.retrieval-fusion/1";

const sha256 = (value) => createHash("sha256").update(value).digest("hex");
const queryInput = (text) => `${QUERY_INSTRUCTION}\nQuery: ${text}`;

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

function compareEpisodeThenText(a, b) {
  return a.episode - b.episode || a.text.localeCompare(b.text);
}

/** Validates and indexes the pilot's vectors.json `entries: [input, vector][]`. */
export function makeVectorLookup(doc) {
  assert(doc && Array.isArray(doc.entries), "vectors: missing entries[]");
  const vectors = new Map();
  let dimensions = null;

  for (const [index, entry] of doc.entries.entries()) {
    assert(Array.isArray(entry) && entry.length === 2, `vectors: entry ${index} must be [inputText, vector]`);
    const [input, vector] = entry;
    assert(typeof input === "string", `vectors: entry ${index} inputText must be a string`);
    assert(Array.isArray(vector) && vector.length > 0, `vectors: entry ${index} vector must be non-empty`);
    assert(vector.every(Number.isFinite), `vectors: entry ${index} vector contains a non-finite value`);
    if (dimensions === null) dimensions = vector.length;
    assert(vector.length === dimensions, `vectors: entry ${index} has dimension ${vector.length}, expected ${dimensions}`);
    assert(!vectors.has(input), `vectors: duplicate inputText at entry ${index}`);
    vectors.set(input, vector);
  }
  return vectors;
}

function vectorFor(vectors, input) {
  const vector = vectors.get(input);
  if (!vector) throw new Error(`Missing vector for input text: ${JSON.stringify(input)}`);
  return vector;
}

export function cosine(left, right) {
  assert(left.length === right.length, `Vector dimensions differ: ${left.length} and ${right.length}`);
  let dot = 0;
  let leftNorm = 0;
  let rightNorm = 0;
  for (let i = 0; i < left.length; i += 1) {
    dot += left[i] * right[i];
    leftNorm += left[i] * left[i];
    rightNorm += right[i] * right[i];
  }
  assert(leftNorm > 0 && rightNorm > 0, "Cannot calculate cosine for a zero vector");
  return dot / Math.sqrt(leftNorm * rightNorm);
}

/**
 * Scores rows, collapses claims to their maximum score per Episode, and emits
 * the complete deterministic Episode ranking. A row lacking a captured vector
 * always throws rather than receiving a default score.
 */
export function rankEpisodes({ rows, queryVector, vectors }) {
  assert(Array.isArray(rows), "rows must be an array");
  assert(Array.isArray(queryVector), "queryVector must be an array");
  const byEpisode = new Map();

  for (const row of rows) {
    assert(Number.isInteger(row?.episode) && row.episode >= 0, "row episode must be a non-negative integer");
    assert(typeof row.text === "string", "row text must be a string");
    const score = cosine(queryVector, vectorFor(vectors, row.text));
    const current = byEpisode.get(row.episode);
    if (!current || score > current.score || (score === current.score && row.text.localeCompare(current.text) < 0)) {
      byEpisode.set(row.episode, { episode: row.episode, text: row.text, score });
    }
  }

  return [...byEpisode.values()]
    .sort((a, b) => b.score - a.score || compareEpisodeThenText(a, b))
    .map((row, index) => ({ ...row, rank: index + 1 }));
}

/** Fixed, equal-weight RRF. It is intentionally not tuned against this pilot. */
export function rrfFuse(originalRanks, englishRanks, { k = RRF_K, weight = RRF_WEIGHT } = {}) {
  assert(Number.isFinite(k) && k >= 0, "RRF k must be non-negative");
  assert(Number.isFinite(weight) && weight > 0 && weight < 1, "RRF weight must be between zero and one");
  const fused = new Map();
  const add = (rows, listWeight) => {
    for (const row of rows) {
      assert(Number.isInteger(row.episode) && row.episode >= 0, "RRF row episode must be a non-negative integer");
      assert(Number.isInteger(row.rank) && row.rank > 0, "RRF row rank must be a positive integer");
      const current = fused.get(row.episode) ?? { episode: row.episode, score: 0 };
      current.score += listWeight / (k + row.rank);
      fused.set(row.episode, current);
    }
  };
  add(originalRanks, weight);
  add(englishRanks, 1 - weight);

  return [...fused.values()]
    .sort((a, b) => b.score - a.score || a.episode - b.episode)
    .map((row, index) => ({ ...row, rank: index + 1 }));
}

function manifestSamples(manifest) {
  assert(manifest && Array.isArray(manifest.samples), "manifest: missing samples[]");
  const ids = new Set();
  return manifest.samples.map((sample, episode) => {
    assert(typeof sample?.id === "string" && sample.id.length > 0, `manifest: sample ${episode} missing id`);
    assert(typeof sample.content === "string", `manifest: sample ${episode} missing content`);
    assert(!ids.has(sample.id), `manifest: duplicate sample id ${sample.id}`);
    ids.add(sample.id);
    return { episode, id: sample.id, text: sample.content };
  });
}

/** Returns only claims admitted after exact quote-issue filtering. */
export function buildFactRows(manifest, receipt) {
  const samples = manifestSamples(manifest);
  assert(Array.isArray(receipt?.result?.episodes), "receipt: missing result.episodes[]");
  assert(Array.isArray(receipt.issues ?? []), "receipt: issues must be an array when present");
  const episodeById = new Map(samples.map((sample) => [sample.id, sample.episode]));
  const invalidQuotes = new Set(
    receipt.issues
      .filter((issue) => issue?.kind === "quote" && typeof issue.id === "string" && Number.isInteger(issue.index))
      .map((issue) => `${issue.id}\u0000${issue.index}`),
  );
  const seenEpisodes = new Set();
  const rows = [];

  for (const sourceEpisode of receipt.result.episodes) {
    const id = sourceEpisode?.episode_id;
    assert(typeof id === "string" && episodeById.has(id), `receipt: unknown episode_id ${JSON.stringify(id)}`);
    assert(!seenEpisodes.has(id), `receipt: duplicate episode_id ${id}`);
    seenEpisodes.add(id);
    assert(Array.isArray(sourceEpisode.claims), `receipt: ${id} missing claims[]`);

    for (const [claimIndex, claim] of sourceEpisode.claims.entries()) {
      assert(typeof claim?.content === "string", `receipt: ${id} claim ${claimIndex} missing content`);
      if (invalidQuotes.has(`${id}\u0000${claimIndex}`)) continue;
      rows.push({ episode: episodeById.get(id), text: claim.content, episode_id: id, claim_index: claimIndex });
    }
  }
  return rows;
}

function validateQueries(doc, sampleCount, { requirePilotShape = false } = {}) {
  assert(doc && Array.isArray(doc.queries), "queries: missing queries[]");
  if (requirePilotShape) {
    assert(sampleCount === 8, `pilot manifest must contain 8 Episodes, found ${sampleCount}`);
    assert(doc.queries.length === 20, `pilot queries must contain 20 queries, found ${doc.queries.length}`);
  }
  const ids = new Set();
  return doc.queries.map((query, index) => {
    assert(typeof query?.id === "string" && query.id.length > 0, `queries: query ${index} missing id`);
    assert(!ids.has(query.id), `queries: duplicate query id ${query.id}`);
    ids.add(query.id);
    assert(typeof query.original === "string", `queries: ${query.id} missing original text`);
    assert(typeof query.english === "string", `queries: ${query.id} missing English text`);
    assert(Number.isInteger(query.episode) && query.episode >= 0 && query.episode < sampleCount, `queries: ${query.id} has invalid target Episode`);
    return query;
  });
}

function rankForQuery(rows, query, language, vectors) {
  return rankEpisodes({ rows, queryVector: vectorFor(vectors, queryInput(query[language])), vectors });
}

function queryRows(condition, queries, rankingsFor) {
  return queries.map((query) => {
    const ranking = rankingsFor(query);
    const target = ranking.find((row) => row.episode === query.episode);
    return {
      query_id: query.id,
      expected_episode: query.episode,
      rank: target?.rank ?? null,
      ranking,
      condition,
    };
  });
}

export function aggregateRanks(rows) {
  const n = rows.length;
  const hits = (limit) => rows.filter((row) => row.rank !== null && row.rank <= limit).length;
  return {
    n,
    hit1: { hits: hits(1), rate: n === 0 ? null : hits(1) / n },
    hit3: { hits: hits(3), rate: n === 0 ? null : hits(3) / n },
    mrr: n === 0 ? null : rows.reduce((sum, row) => sum + (row.rank === null ? 0 : 1 / row.rank), 0) / n,
  };
}

function condition(condition, rows) {
  return { condition, aggregate: aggregateRanks(rows), queries: rows };
}

/**
 * Calculates all requested paths for one English extraction receipt. This is
 * exported so tests can exercise the offline ranking logic without files.
 */
export function compareReceipt({ manifest, queries: queryDoc, receipt, vectors }) {
  const samples = manifestSamples(manifest);
  const queries = validateQueries(queryDoc, samples.length);
  const originals = samples.map(({ episode, text }) => ({ episode, text }));
  const facts = buildFactRows(manifest, receipt);
  const union = [...originals, ...facts];

  const originalsOriginal = queryRows("originals_only_original_query", queries, (query) =>
    rankForQuery(originals, query, "original", vectors),
  );
  const factsEnglish = queryRows("english_facts_only_english_query", queries, (query) =>
    rankForQuery(facts, query, "english", vectors),
  );
  const unionOriginal = queryRows("union_original_and_english_facts_original_query", queries, (query) =>
    rankForQuery(union, query, "original", vectors),
  );
  const unionEnglish = queryRows("union_original_and_english_facts_english_query", queries, (query) =>
    rankForQuery(union, query, "english", vectors),
  );
  const unionRrf = queryRows("union_equal_weight_rrf_original_and_english_query", queries, (query) =>
    rrfFuse(
      rankForQuery(union, query, "original", vectors),
      rankForQuery(union, query, "english", vectors),
    ),
  );

  return {
    admitted_fact_claims: facts.length,
    excluded_exact_quote_claims: receipt.result.episodes.reduce((total, episode) => total + episode.claims.length, 0) - facts.length,
    conditions: [
      condition("originals_only_original_query", originalsOriginal),
      condition("english_facts_only_english_query", factsEnglish),
      condition("union_original_and_english_facts_original_query", unionOriginal),
      condition("union_original_and_english_facts_english_query", unionEnglish),
      condition("union_equal_weight_rrf_original_and_english_query", unionRrf),
    ],
  };
}

export function parseArgs(argv) {
  const args = { manifest: null, vectors: null, queries: null, gpt: null, opus: null, out: null };
  for (let index = 0; index < argv.length; index += 1) {
    const key = argv[index];
    if (!(key in { "--manifest": 1, "--vectors": 1, "--queries": 1, "--gpt": 1, "--opus": 1, "--out": 1 })) {
      throw new Error(`unknown argument ${key}`);
    }
    const value = argv[index + 1];
    if (value === undefined || value.startsWith("--")) throw new Error(`${key} requires a value`);
    args[key.slice(2)] = value;
    index += 1;
  }
  for (const [key, value] of Object.entries(args)) assert(value, `--${key} is required`);
  return args;
}

async function readJsonWithHash(path, name) {
  const raw = await readFile(path, "utf8");
  try {
    return { value: JSON.parse(raw), sha256: sha256(raw) };
  } catch (error) {
    throw new Error(`${name}: invalid JSON: ${error.message}`);
  }
}

/** Reads supplied captures and writes one self-contained, offline JSON report. */
export async function runComparison(options) {
  const [manifestFile, vectorsFile, queriesFile, gptFile, opusFile] = await Promise.all([
    readJsonWithHash(options.manifest, "manifest"),
    readJsonWithHash(options.vectors, "vectors"),
    readJsonWithHash(options.queries, "queries"),
    readJsonWithHash(options.gpt, "gpt receipt"),
    readJsonWithHash(options.opus, "opus receipt"),
  ]);
  const samples = manifestSamples(manifestFile.value);
  validateQueries(queriesFile.value, samples.length, { requirePilotShape: true });
  const vectors = makeVectorLookup(vectorsFile.value);

  const report = {
    schema: REPORT_SCHEMA,
    version: 1,
    description:
      "Offline retrieval fusion comparison over existing captured pilot vectors. No embedding or model calls are made.",
    interpretation_limit:
      "This is the same eight-Episode, 20-query development set used by the extraction pilot. It is not held out and does not establish causal language effects or a final retrieval policy.",
    policy: {
      fact_admission: "Exclude a claim only when its receipt issue has kind exactly \"quote\" and the same episode id and claim index.",
      episode_score: "Maximum cosine similarity among admitted rows for an Episode; ties sort by Episode index, then claim text.",
      paths: [
        "originals-only with original-language query",
        "EnglishFacts-only with English query",
        "UNION(originals, EnglishFacts) with original-language query",
        "UNION(originals, EnglishFacts) with English query",
        "equal-weight RRF(k=60) over the two UNION ranks",
      ],
      rrf: { k: RRF_K, original_query_weight: RRF_WEIGHT, english_query_weight: RRF_WEIGHT, tuned_on_results: false },
    },
    source_hashes: {
      manifest_sha256: manifestFile.sha256,
      vectors_sha256: vectorsFile.sha256,
      queries_sha256: queriesFile.sha256,
      gpt_receipt_sha256: gptFile.sha256,
      opus_receipt_sha256: opusFile.sha256,
    },
    development_set: { episodes: samples.length, queries: queriesFile.value.queries.length },
    results: {
      gpt: compareReceipt({ manifest: manifestFile.value, queries: queriesFile.value, receipt: gptFile.value, vectors }),
      opus: compareReceipt({ manifest: manifestFile.value, queries: queriesFile.value, receipt: opusFile.value, vectors }),
    },
  };

  await mkdir(dirname(options.out), { recursive: true, mode: 0o700 });
  await writeFile(options.out, `${JSON.stringify(report, null, 2)}\n`, { mode: 0o600 });
  return report;
}

async function main() {
  const options = parseArgs(process.argv.slice(2));
  const report = await runComparison(options);
  console.log(`wrote ${options.out}`);
  for (const [model, result] of Object.entries(report.results)) {
    console.log(`${model}: ${result.admitted_fact_claims} admitted facts; ${result.excluded_exact_quote_claims} exact-quote claims excluded`);
    for (const entry of result.conditions) {
      const { hit1, hit3, mrr } = entry.aggregate;
      console.log(`  ${entry.condition}: Hit@1 ${hit1.hits}/${hit1.rate === null ? 0 : entry.aggregate.n}, Hit@3 ${hit3.hits}/${hit3.rate === null ? 0 : entry.aggregate.n}, MRR ${mrr ?? "n/a"}`);
    }
  }
}

if (import.meta.main) await main();
