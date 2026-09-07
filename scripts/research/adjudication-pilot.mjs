#!/usr/bin/env bun
// Bounded offline/live research evaluator for paired GPT-5.5 vs Claude Opus-5
// adjudication over 36 SYNTHETIC specification-conformance cases.
//
// This measures whether a model applies the stated rules of docs/02 §5 (judge
// outcomes), docs/02 §5.1 (modality), docs/03 §5 (change vs correction) and
// D46 (one Fact per occurrence, no automatic winner). It is not a statistical
// certification, not a deployment gate, and not a model-quality score.
//
// Offline:
//   bun scripts/research/adjudication-pilot.mjs \
//     --fixtures scripts/research/adjudication-cases.json \
//     --model gpt-5.5 --replay <capture-dir> --out-dir <out>
//
// Dependencies: Bun/Node builtins only.

import { createHash } from "node:crypto";
import { mkdir, readFile, writeFile } from "node:fs/promises";
import { basename, join } from "node:path";
import { fileURLToPath } from "node:url";

const FILE_MODE = 0o600;
const DIR_MODE = 0o700;
const DEFAULT_TIMEOUT_MS = 120_000;

const sha256 = (value) => createHash("sha256").update(value).digest("hex");

export const MODELS = ["gpt-5.5", "claude-opus-5"];

export const VERDICTS = [
  "NEW",
  "DUPLICATE_OCCURRENCE",
  "ELABORATION",
  "CHANGE",
  "CORRECTION",
  "UNRESOLVED_CONTRADICTION",
];

/** Verdicts that write an INVALIDATES edge (docs/02 §5 L4, docs/03 §5). */
export const INVALIDATING_VERDICTS = new Set(["CHANGE", "CORRECTION"]);

export const TIME_BASES = ["proposed", "target", null];

export const GOLD_FIELD_NAMES = [
  "gold_verdict",
  "gold_target_ids",
  "gold_evidence_ids",
  "gold_effective_time_basis",
  "gold_time_basis_normative",
  "gold_rationale",
];

export const FAILURE_KINDS = [
  "transport",
  "http",
  "parse",
  "schema",
  "target_invalid",
  "evidence_invalid",
];

/**
 * Gold labels observed to be arguable once real model answers arrived. Recorded
 * so a reader discounts the affected counts; the labels themselves are left
 * alone, because retuning gold against results would make the counts
 * unfalsifiable.
 */
export const INTERPRETATION_LIMITS = [
  {
    case_id: "new-en-004",
    field: "gold_verdict",
    note: "Gold is NEW; ELABORATION is defensible on the same text. A miss here may reflect the NEW/ELABORATION boundary rather than a conformance error.",
  },
  {
    case_id: "unr-en-501",
    field: "gold_evidence_ids",
    note: "Gold cites the minimal span; the additional snippet s2 is substantively relevant, so evidence_exact can understate an answer that cited more context than required.",
  },
];

/** Bound on `reason`, enforced locally rather than on the wire. */
export const REASON_MAX_LENGTH = 320;

/**
 * The JSON schema handed to both native endpoints.
 *
 * `reason` carries no `maxLength`: the shared wire schema omits the length
 * constraint, while validateModelOutput enforces REASON_MAX_LENGTH locally.
 * This raw-fetch runner performs no SDK schema transformation. Removing the
 * length keyword alone did not resolve the observed Messages HTTP 400.
 *
 * `effective_time_basis` is nullable through `anyOf`, not through a union
 * `type` array with `null` inside a mixed `enum`. Isolated against the captured
 * request: with `maxLength` already removed, the union+mixed-enum encoding
 * returns 400 from /v1/messages and the `anyOf` encoding returns 200. The
 * accepted value set is identical either way, including a real JSON null.
 */
export const OUTPUT_SCHEMA = {
  type: "object",
  additionalProperties: false,
  required: ["verdict", "target_ids", "evidence_ids", "effective_time_basis", "reason"],
  properties: {
    verdict: { type: "string", enum: VERDICTS },
    target_ids: { type: "array", items: { type: "string" } },
    evidence_ids: { type: "array", items: { type: "string" } },
    effective_time_basis: {
      anyOf: [{ type: "string", enum: ["proposed", "target"] }, { type: "null" }],
    },
    reason: { type: "string" },
  },
};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

export function loadCases(doc) {
  if (!doc || !Array.isArray(doc.cases)) throw new Error("fixtures: missing cases[]");
  if (doc.provenance !== "SYNTHETIC") throw new Error("fixtures: provenance must be SYNTHETIC");

  const seen = new Set();
  for (const kase of doc.cases) {
    if (!kase.id) throw new Error("fixtures: case without id");
    if (seen.has(kase.id)) throw new Error(`fixtures: duplicate case id ${kase.id}`);
    seen.add(kase.id);

    if (!VERDICTS.includes(kase.gold_verdict)) {
      throw new Error(`fixtures: ${kase.id} has unknown gold_verdict`);
    }
    if (!Array.isArray(kase.gold_target_ids)) {
      throw new Error(`fixtures: ${kase.id} missing gold_target_ids`);
    }
    if (!Array.isArray(kase.gold_evidence_ids) || kase.gold_evidence_ids.length === 0) {
      throw new Error(`fixtures: ${kase.id} missing gold_evidence_ids`);
    }
    if (typeof kase.gold_time_basis_normative !== "boolean") {
      throw new Error(`fixtures: ${kase.id} missing gold_time_basis_normative`);
    }
    if (!TIME_BASES.includes(kase.gold_effective_time_basis)) {
      throw new Error(`fixtures: ${kase.id} has unknown gold_effective_time_basis`);
    }

    const candidateIds = new Set((kase.candidates ?? []).map((c) => c.id));
    for (const id of kase.gold_target_ids) {
      if (!candidateIds.has(id)) throw new Error(`fixtures: ${kase.id} gold target ${id} unknown`);
    }
    const snippetIds = new Set(kase.episode.snippets.map((s) => s.id));
    for (const id of kase.gold_evidence_ids) {
      if (!snippetIds.has(id)) throw new Error(`fixtures: ${kase.id} gold evidence ${id} unknown`);
    }
  }
  return doc.cases;
}

/**
 * The exact object handed to the model.
 *
 * Gold fields never appear here, and neither do the case, episode or claim ids:
 * those encode the gold label in their stem (new-, dup-, elab-, chg-, cor-,
 * unr-) and the model has no use for them. Candidate and snippet ids are
 * preserved because the model must cite them back. Supplied normalization is
 * passed through unchanged — the adjudicator selects a basis, it does not
 * re-derive a time.
 */
export function buildModelPayload(kase) {
  const claim = kase.proposed_claim;
  return {
    episode: {
      time_value: kase.episode.time_value,
      time_utc: kase.episode.time_utc,
      snippets: kase.episode.snippets.map((s) => ({
        id: s.id,
        speaker: s.speaker,
        text: s.text,
      })),
    },
    proposed_claim: {
      content: claim.content,
      modality: claim.modality,
      time_value: claim.time_value,
      time_utc: claim.time_utc,
      time_precision: claim.time_precision,
      entities: claim.entities,
    },
    candidates: (kase.candidates ?? []).map((c) => ({
      id: c.id,
      content: c.content,
      modality: c.modality,
      time_value: c.time_value,
      time_utc: c.time_utc,
      time_precision: c.time_precision,
      valid: c.valid,
      ...(c.invalidates ? { invalidates: c.invalidates } : {}),
    })),
  };
}

// ---------------------------------------------------------------------------
// Native requests
// ---------------------------------------------------------------------------

export function buildRequest({ model, baseUrl, bearer, prompt, payload, maxTokens = 1024 }) {
  const root = String(baseUrl ?? "").replace(/\/+$/, "");
  const userText = `${prompt}\n\n## Case\n\n\`\`\`json\n${JSON.stringify(payload, null, 2)}\n\`\`\``;

  if (model === "gpt-5.5") {
    return {
      url: `${root}/v1/responses`,
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "text/event-stream",
        authorization: `Bearer ${bearer ?? ""}`,
      },
      body: {
        model,
        input: [{ role: "user", content: [{ type: "input_text", text: userText }] }],
        reasoning: { effort: "none" },
        stream: true,
        store: false,
        max_output_tokens: maxTokens,
        text: {
          format: {
            type: "json_schema",
            name: "adjudication_verdict",
            strict: true,
            schema: OUTPUT_SCHEMA,
          },
        },
      },
    };
  }

  if (model === "claude-opus-5") {
    return {
      url: `${root}/v1/messages`,
      method: "POST",
      headers: {
        "content-type": "application/json",
        accept: "text/event-stream",
        authorization: `Bearer ${bearer ?? ""}`,
        "anthropic-version": "2023-06-01",
      },
      body: {
        model,
        max_tokens: maxTokens,
        stream: true,
        thinking: { type: "disabled" },
        messages: [{ role: "user", content: [{ type: "text", text: userText }] }],
        // Only `type` and `schema`; the endpoint rejects an unknown `name`.
        output_config: {
          format: {
            type: "json_schema",
            schema: OUTPUT_SCHEMA,
          },
        },
      },
    };
  }

  throw new Error(`unknown model ${model}`);
}

/** Everything persisted or logged goes through this first. */
export function redactRequest(req) {
  const headers = {};
  for (const key of Object.keys(req.headers ?? {})) {
    const lower = key.toLowerCase();
    headers[lower] =
      lower === "authorization" || lower === "x-api-key" ? "[redacted]" : req.headers[key];
  }
  return { url: req.url, method: req.method, headers, body: req.body };
}

// ---------------------------------------------------------------------------
// Stream parsing
// ---------------------------------------------------------------------------

export class StreamParseError extends Error {}

function sseData(body) {
  const events = [];
  for (const line of String(body).split(/\r?\n/)) {
    if (!line.startsWith("data:")) continue;
    const raw = line.slice(5).trim();
    if (!raw || raw === "[DONE]") continue;
    try {
      events.push(JSON.parse(raw));
    } catch {
      // A non-JSON data line is not itself fatal; the terminal event decides.
    }
  }
  return events;
}

/**
 * /v1/responses: read `response.output_text.done`, because `response.completed`
 * may carry an empty `output` array. A terminal `response.completed` with
 * status `completed` is still required: a truncated or failed stream must not
 * be scored as an answer.
 */
export function parseResponsesSse(body) {
  const events = sseData(body);
  if (events.length === 0) throw new StreamParseError("responses: no SSE data events");

  let text = null;
  let usage = null;
  let model = null;
  let deltas = "";
  let completed = false;

  for (const ev of events) {
    if (ev.response?.model) model = ev.response.model;

    switch (ev.type) {
      case "response.output_text.done":
        if (typeof ev.text === "string" && ev.text.length > 0) text = ev.text;
        break;
      case "response.output_text.delta":
        if (typeof ev.delta === "string") deltas += ev.delta;
        break;
      case "response.completed": {
        usage = ev.response?.usage ?? usage;
        const status = ev.response?.status;
        if (status !== undefined && status !== "completed") {
          throw new StreamParseError(`responses: terminal status ${status}`);
        }
        completed = true;
        if (text === null) {
          const parts = [];
          for (const item of ev.response?.output ?? []) {
            for (const chunk of item.content ?? []) {
              if (typeof chunk.text === "string") parts.push(chunk.text);
            }
          }
          if (parts.length > 0) text = parts.join("");
        }
        break;
      }
      case "response.incomplete":
      case "response.failed":
      case "error":
        throw new StreamParseError(`responses: stream reported ${ev.type}`);
      default:
        break;
    }
  }

  if (!completed) throw new StreamParseError("responses: stream ended without response.completed");
  if (text === null) {
    throw new StreamParseError("responses: no output_text.done and empty completed.output");
  }
  return { text, usage, model, partial_deltas: deltas };
}

/**
 * /v1/messages: accumulate text deltas. `message_stop` must terminate the
 * stream and the stop reason must be `end_turn`; a `max_tokens` stop is a
 * truncated answer, not a verdict.
 */
export function parseMessagesSse(body) {
  const events = sseData(body);
  if (events.length === 0) throw new StreamParseError("messages: no SSE data events");

  let text = "";
  let stopped = false;
  let usage = null;
  let model = null;
  let stopReason = null;

  for (const ev of events) {
    switch (ev.type) {
      case "message_start":
        usage = { ...(ev.message?.usage ?? {}) };
        if (ev.message?.model) model = ev.message.model;
        if (ev.message?.stop_reason) stopReason = ev.message.stop_reason;
        break;
      case "content_block_delta":
        if (ev.delta?.type === "text_delta" && typeof ev.delta.text === "string") {
          text += ev.delta.text;
        }
        break;
      case "message_delta":
        usage = { ...(usage ?? {}), ...(ev.usage ?? {}) };
        if (ev.delta?.stop_reason) stopReason = ev.delta.stop_reason;
        break;
      case "message_stop":
        stopped = true;
        break;
      case "error":
        throw new StreamParseError("messages: stream reported error");
      default:
        break;
    }
  }

  if (!stopped) throw new StreamParseError("messages: stream ended without message_stop");
  if (stopReason !== "end_turn") {
    throw new StreamParseError(`messages: stop_reason ${stopReason ?? "missing"}`);
  }
  if (text.length === 0) throw new StreamParseError("messages: no text deltas");
  return { text, usage, model, partial_deltas: text };
}

export function parseStream(model, body) {
  return model === "claude-opus-5" ? parseMessagesSse(body) : parseResponsesSse(body);
}

// ---------------------------------------------------------------------------
// Output validation
// ---------------------------------------------------------------------------

/** Enforces exactly the declared schema: required keys, no extras, limits. */
export function validateModelOutput(value) {
  const errors = [];
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return { ok: false, errors: ["output is not an object"] };
  }

  const declared = OUTPUT_SCHEMA.required;
  for (const key of declared) {
    if (!Object.hasOwn(value, key)) errors.push(`missing required key ${key}`);
  }
  for (const key of Object.keys(value)) {
    if (!declared.includes(key)) errors.push(`undeclared key ${key}`);
  }

  if (!VERDICTS.includes(value.verdict)) errors.push("verdict not in enum");
  if (!Array.isArray(value.target_ids) || value.target_ids.some((v) => typeof v !== "string")) {
    errors.push("target_ids must be a string array");
  }
  if (!Array.isArray(value.evidence_ids) || value.evidence_ids.some((v) => typeof v !== "string")) {
    errors.push("evidence_ids must be a string array");
  }
  if (Object.hasOwn(value, "effective_time_basis") && !TIME_BASES.includes(value.effective_time_basis)) {
    errors.push("effective_time_basis not in enum");
  }
  if (typeof value.reason !== "string") errors.push("reason must be a string");
  else if (value.reason.length > REASON_MAX_LENGTH) {
    // The shared wire schema omits this bound; local validation enforces it.
    errors.push("reason exceeds maxLength");
  }

  return { ok: errors.length === 0, errors };
}

/** Referential integrity against this case's own ids. */
export function checkReferences(kase, output) {
  const candidateIds = new Set((kase.candidates ?? []).map((c) => c.id));
  const snippetIds = new Set(kase.episode.snippets.map((s) => s.id));
  const unknownTargets = output.target_ids.filter((id) => !candidateIds.has(id));
  const unknownEvidence = output.evidence_ids.filter((id) => !snippetIds.has(id));
  return { unknownTargets, unknownEvidence };
}

// ---------------------------------------------------------------------------
// Scoring
// ---------------------------------------------------------------------------

function sameSet(a, b) {
  const left = new Set(a);
  const right = new Set(b);
  if (left.size !== right.size) return false;
  for (const v of left) if (!right.has(v)) return false;
  return true;
}

/**
 * Scores one model answer. Verdict, exact target set and exact evidence set are
 * always scored. The time basis is a selection field: it is only a scored
 * mode/time error where docs/03 §5 pins it (CHANGE, CORRECTION); elsewhere the
 * comparison is advisory.
 */
export function scoreCase(kase, output) {
  const verdict_exact = output.verdict === kase.gold_verdict;
  const target_exact = sameSet(output.target_ids, kase.gold_target_ids);
  const evidence_exact = sameSet(output.evidence_ids, kase.gold_evidence_ids);
  const basis = output.effective_time_basis ?? null;
  const time_basis_exact = basis === kase.gold_effective_time_basis;
  const time_basis_normative = kase.gold_time_basis_normative === true;

  // A change/correction mix-up is a mode error even when the basis string
  // happens to match the (wrong) verdict the model chose.
  const modeConfusion =
    INVALIDATING_VERDICTS.has(kase.gold_verdict) &&
    INVALIDATING_VERDICTS.has(output.verdict) &&
    output.verdict !== kase.gold_verdict;

  const time_basis_error = (time_basis_normative && !time_basis_exact) || modeConfusion;

  const core_exact = verdict_exact && target_exact && !time_basis_error;
  const full_exact = core_exact && evidence_exact && time_basis_exact;

  return {
    case_id: kase.id,
    label: kase.gold_verdict,
    lang: kase.lang,
    status: "scored",
    failure_kind: null,
    verdict: output.verdict,
    target_ids: [...output.target_ids],
    evidence_ids: [...output.evidence_ids],
    effective_time_basis: basis,
    reason: typeof output.reason === "string" ? output.reason : null,
    verdict_exact,
    target_exact,
    evidence_exact,
    time_basis_exact,
    time_basis_normative,
    time_basis_error,
    core_exact,
    full_exact,
  };
}

function failedRow(kase, { failure_kind, http_status = null, error = null, raw_output = "", latency_ms = null, usage = null, resolved_model = null }) {
  return {
    case_id: kase.id,
    label: kase.gold_verdict,
    lang: kase.lang,
    status: "failed",
    failure_kind,
    http_status,
    error,
    latency_ms,
    usage,
    resolved_model,
    raw_output,
    verdict: null,
    target_ids: [],
    evidence_ids: [],
    effective_time_basis: null,
    reason: null,
    verdict_exact: false,
    target_exact: false,
    evidence_exact: false,
    time_basis_exact: false,
    time_basis_normative: kase.gold_time_basis_normative === true,
    time_basis_error: false,
    core_exact: false,
    full_exact: false,
  };
}

// ---------------------------------------------------------------------------
// One case, one terminal row, no retries
// ---------------------------------------------------------------------------

export async function runCase({ kase, model, transport, prompt = "", baseUrl = "", bearer = null, onCapture = null }) {
  const payload = buildModelPayload(kase);
  const request = buildRequest({ model, baseUrl, bearer, prompt, payload });
  const started = Date.now();

  let response;
  try {
    response = await transport(request);
  } catch (err) {
    const row = failedRow(kase, {
      failure_kind: "transport",
      error: String(err?.message ?? err),
      latency_ms: Date.now() - started,
    });
    if (onCapture) await onCapture({ kase, request, row, raw: "" });
    return row;
  }

  const latency_ms = response.latency_ms ?? Date.now() - started;
  const raw = typeof response.body === "string" ? response.body : "";
  const http_status = response.status ?? null;

  const finish = async (row) => {
    if (onCapture) await onCapture({ kase, request, row, raw });
    return row;
  };

  if (typeof http_status !== "number" || http_status < 200 || http_status >= 300) {
    return finish(failedRow(kase, { failure_kind: "http", http_status, raw_output: raw, latency_ms }));
  }

  let parsed;
  try {
    parsed = parseStream(model, raw);
  } catch (err) {
    return finish(
      failedRow(kase, {
        failure_kind: "parse",
        http_status,
        error: String(err?.message ?? err),
        raw_output: raw,
        latency_ms,
      }),
    );
  }

  let value;
  try {
    value = JSON.parse(parsed.text);
  } catch (err) {
    return finish(
      failedRow(kase, {
        failure_kind: "parse",
        http_status,
        error: `model text is not JSON: ${String(err?.message ?? err)}`,
        raw_output: raw,
        latency_ms,
        usage: parsed.usage,
      }),
    );
  }

  const schema = validateModelOutput(value);
  if (!schema.ok) {
    return finish(
      failedRow(kase, {
        failure_kind: "schema",
        http_status,
        error: schema.errors.join("; "),
        raw_output: raw,
        latency_ms,
        usage: parsed.usage,
      }),
    );
  }

  const refs = checkReferences(kase, value);
  if (refs.unknownTargets.length > 0) {
    return finish(
      failedRow(kase, {
        failure_kind: "target_invalid",
        http_status,
        error: `unknown target ids: ${refs.unknownTargets.join(", ")}`,
        raw_output: raw,
        latency_ms,
        usage: parsed.usage,
      }),
    );
  }
  if (refs.unknownEvidence.length > 0) {
    return finish(
      failedRow(kase, {
        failure_kind: "evidence_invalid",
        http_status,
        error: `unknown evidence ids: ${refs.unknownEvidence.join(", ")}`,
        raw_output: raw,
        latency_ms,
        usage: parsed.usage,
      }),
    );
  }

  const row = {
    ...scoreCase(kase, value),
    http_status,
    latency_ms,
    usage: parsed.usage ?? null,
    resolved_model: parsed.model ?? null,
    error: null,
    raw_output: raw,
  };
  return finish(row);
}

// ---------------------------------------------------------------------------
// Aggregate
// ---------------------------------------------------------------------------

export function aggregate({
  rows,
  expected,
  model,
  fixtures = null,
  promptSha256 = null,
  fixturesSha256 = null,
}) {
  const per_label = {};
  for (const label of VERDICTS) {
    per_label[label] = {
      n: 0,
      verdict_exact: 0,
      target_exact: 0,
      evidence_exact: 0,
      time_basis_exact: 0,
      core_exact: 0,
      full_exact: 0,
      failed: 0,
    };
  }

  const by_kind = {};
  for (const kind of FAILURE_KINDS) by_kind[kind] = 0;

  let scored = 0;
  let failuresTotal = 0;
  let false_invalidation = 0;
  let missed_invalidation = 0;
  let wrong_target_given_verdict = 0;
  let target_mismatch_total = 0;
  let wrong_mode_or_time = 0;
  let unresolved_missed = 0;
  let unresolved_invalidated = 0;
  let advisory_basis_mismatch = 0;

  for (const row of rows) {
    const bucket = per_label[row.label];
    bucket.n += 1;

    if (row.status === "failed") {
      failuresTotal += 1;
      bucket.failed += 1;
      if (row.failure_kind in by_kind) by_kind[row.failure_kind] += 1;
      else by_kind[row.failure_kind] = 1;
      continue;
    }

    scored += 1;
    if (row.verdict_exact) bucket.verdict_exact += 1;
    if (row.target_exact) bucket.target_exact += 1;
    if (row.evidence_exact) bucket.evidence_exact += 1;
    if (row.time_basis_exact) bucket.time_basis_exact += 1;
    if (row.core_exact) bucket.core_exact += 1;
    if (row.full_exact) bucket.full_exact += 1;

    const goldInvalidates = INVALIDATING_VERDICTS.has(row.label);
    const saidInvalidates = INVALIDATING_VERDICTS.has(row.verdict);
    if (!goldInvalidates && saidInvalidates) false_invalidation += 1;
    if (goldInvalidates && !saidInvalidates) missed_invalidation += 1;
    // Two distinct target views: mismatches on rows that got the verdict right
    // (the verdict is usable, the anchor is not) and mismatches overall.
    if (!row.target_exact) target_mismatch_total += 1;
    if (row.verdict_exact && !row.target_exact) wrong_target_given_verdict += 1;
    if (row.time_basis_error) wrong_mode_or_time += 1;
    // Gold UNRESOLVED split by consequence: predicting a non-invalidating
    // verdict misses the conflict and writes nothing, which is not the same
    // failure as electing a winner and invalidating the other side.
    if (row.label === "UNRESOLVED_CONTRADICTION" && row.verdict !== "UNRESOLVED_CONTRADICTION") {
      if (INVALIDATING_VERDICTS.has(row.verdict)) unresolved_invalidated += 1;
      else unresolved_missed += 1;
    }
    if (!row.time_basis_normative && !row.time_basis_exact) advisory_basis_mismatch += 1;
  }

  return {
    schema: "anamnesis.research.adjudication-report/1",
    provenance: "SYNTHETIC",
    caveat:
      "Specification-conformance counts over 36 synthetic cases. Not a statistical estimate, not human-labeled production data, and not a deployment certification.",
    interpretation_limits: INTERPRETATION_LIMITS,
    model,
    resolved_models: [...new Set(rows.map((r) => r.resolved_model).filter(Boolean))].sort(),
    fixtures: fixtures ? basename(fixtures) : null,
    prompt_sha256: promptSha256,
    fixtures_sha256: fixturesSha256,
    generated_at: new Date().toISOString(),
    expected,
    attempted: rows.length,
    complete: rows.length === expected,
    scored,
    per_label,
    false_invalidation,
    missed_invalidation,
    wrong_target_given_verdict,
    target_mismatch_total,
    wrong_mode_or_time,
    unresolved_missed,
    unresolved_invalidated,
    unresolved_misclassified: unresolved_missed + unresolved_invalidated,
    advisory_basis_mismatch,
    failures: { total: failuresTotal, by_kind },
  };
}

// ---------------------------------------------------------------------------
// CLI
// ---------------------------------------------------------------------------

export function parseArgs(argv) {
  const out = {
    fixtures: null,
    model: null,
    keyFile: null,
    outDir: null,
    replay: null,
    concurrency: 2,
    prompt: null,
    only: null,
    timeoutMs: DEFAULT_TIMEOUT_MS,
  };

  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i];
    const next = () => {
      const v = argv[i + 1];
      if (v === undefined) throw new Error(`${arg} requires a value`);
      i += 1;
      return v;
    };
    switch (arg) {
      case "--fixtures": out.fixtures = next(); break;
      case "--model": out.model = next(); break;
      case "--key-file": out.keyFile = next(); break;
      case "--out-dir": out.outDir = next(); break;
      case "--replay": out.replay = next(); break;
      case "--concurrency": out.concurrency = Number(next()); break;
      case "--timeout-ms": out.timeoutMs = Number(next()); break;
      case "--prompt": out.prompt = next(); break;
      case "--only": out.only = next().split(",").map((s) => s.trim()).filter(Boolean); break;
      default:
        throw new Error(`unknown argument ${arg}`);
    }
  }

  if (!out.fixtures) throw new Error("--fixtures is required");
  if (!MODELS.includes(out.model)) throw new Error(`--model must be one of ${MODELS.join("|")}`);
  if (!out.outDir) throw new Error("--out-dir is required");
  if (!Number.isInteger(out.concurrency) || out.concurrency < 1) {
    throw new Error("--concurrency must be a positive integer");
  }
  if (!Number.isInteger(out.timeoutMs) || out.timeoutMs < 1) {
    throw new Error("--timeout-ms must be a positive integer");
  }
  if (!out.replay && !out.keyFile) {
    throw new Error("--key-file is required for a live run (use --replay to stay offline)");
  }
  return out;
}

/** Credential JSON {bearer, base_url}. Read only on a live run. */
export async function readCredentials(keyFile) {
  const parsed = JSON.parse(await readFile(keyFile, "utf8"));
  if (!parsed.bearer || typeof parsed.bearer !== "string") {
    throw new Error("key file: missing string field `bearer`");
  }
  if (!parsed.base_url || typeof parsed.base_url !== "string") {
    throw new Error("key file: missing string field `base_url`");
  }
  return { bearer: parsed.bearer, baseUrl: parsed.base_url };
}

/** Live transport with a bounded timeout. No retries. */
export function createFetchTransport({ timeoutMs = DEFAULT_TIMEOUT_MS, fetchImpl = fetch } = {}) {
  return async (request) => {
    const started = Date.now();
    const controller = new AbortController();
    const timer = setTimeout(
      () => controller.abort(new Error(`request exceeded ${timeoutMs}ms`)),
      timeoutMs,
    );
    try {
      const res = await fetchImpl(request.url, {
        method: request.method,
        headers: request.headers,
        body: JSON.stringify(request.body),
        signal: controller.signal,
      });
      const body = await res.text();
      return { status: res.status, body, latency_ms: Date.now() - started };
    } finally {
      clearTimeout(timer);
    }
  };
}

/**
 * Offline transport: read a body captured by this runner. The capture format
 * is the replay format - `http_status`, `body`, `transport_error` - so a live
 * run's captures directory replays without any hand editing.
 */
function replayTransport(dir) {
  return async (kase) => {
    const path = join(dir, `${kase.id}.capture.json`);
    const capture = JSON.parse(await readFile(path, "utf8"));
    if (capture.transport_error) throw new Error(capture.transport_error);
    return {
      status: capture.http_status ?? 200,
      body: capture.body ?? "",
      latency_ms: capture.latency_ms ?? 0,
    };
  };
}

async function boundedMap(items, limit, worker) {
  const results = new Array(items.length);
  let cursor = 0;
  const lanes = Array.from({ length: Math.min(limit, items.length) }, async () => {
    while (cursor < items.length) {
      const index = cursor;
      cursor += 1;
      results[index] = await worker(items[index], index);
    }
  });
  await Promise.all(lanes);
  return results;
}

export async function runPilot(options) {
  const {
    fixtures,
    model,
    outDir,
    replay = null,
    keyFile = null,
    concurrency = 2,
    prompt = null,
    only = null,
    transport = null,
    timeoutMs = DEFAULT_TIMEOUT_MS,
  } = options;

  const fixturesRaw = await readFile(fixtures, "utf8");
  const doc = JSON.parse(fixturesRaw);
  let cases = loadCases(doc);
  if (only) {
    const wanted = new Set(only);
    cases = cases.filter((c) => wanted.has(c.id));
  }

  const promptPath = prompt ?? fileURLToPath(new URL("adjudication-prompt.md", import.meta.url));
  const promptText = await readFile(promptPath, "utf8");

  let bearer = null;
  let baseUrl = "";
  let call;

  if (replay) {
    const replayFor = replayTransport(replay);
    call = (kase) => (request) => replayFor(kase, request);
  } else {
    const creds = await readCredentials(keyFile);
    bearer = creds.bearer;
    baseUrl = creds.baseUrl;
    const live = transport ?? createFetchTransport({ timeoutMs });
    call = () => live;
  }

  const captureDir = join(outDir, "captures");
  await mkdir(captureDir, { recursive: true, mode: DIR_MODE });

  // Written in the shape replayTransport reads, so captures round-trip.
  const onCapture = async ({ kase, request, row, raw }) => {
    const capture = {
      case_id: kase.id,
      model,
      resolved_model: row.resolved_model ?? null,
      request: redactRequest(request),
      http_status: row.http_status ?? null,
      transport_error: row.failure_kind === "transport" ? (row.error ?? "transport failure") : null,
      body: raw,
      latency_ms: row.latency_ms ?? null,
      status: row.status,
      failure_kind: row.failure_kind,
      error: row.error ?? null,
      usage: row.usage ?? null,
    };
    await writeFile(join(captureDir, `${kase.id}.capture.json`), JSON.stringify(capture, null, 2), {
      mode: FILE_MODE,
    });
  };

  const rows = await boundedMap(cases, concurrency, (kase) =>
    runCase({
      kase,
      model,
      transport: call(kase),
      prompt: promptText,
      baseUrl,
      bearer,
      onCapture,
    }),
  );

  const report = aggregate({
    rows,
    expected: cases.length,
    model,
    fixtures,
    promptSha256: sha256(promptText),
    fixturesSha256: sha256(fixturesRaw),
  });

  const rowsPath = join(outDir, "rows.jsonl");
  const aggPath = join(outDir, "aggregate.json");
  await writeFile(
    rowsPath,
    `${rows.map((r) => JSON.stringify({ ...r, raw_output: undefined })).join("\n")}\n`,
    { mode: FILE_MODE },
  );
  await writeFile(aggPath, `${JSON.stringify(report, null, 2)}\n`, { mode: FILE_MODE });

  return { rows, aggregate: report, rowsPath, aggregatePath: aggPath, captureDir };
}

/** Console summary. Never prints credentials, headers or raw bodies. */
export function summarize(report) {
  const lines = [
    `model: ${report.model}`,
    `fixtures: ${report.fixtures} (SYNTHETIC specification-conformance, not certification)`,
    `resolved model(s): ${report.resolved_models.join(", ") || "none"}`,
    `prompt sha256: ${report.prompt_sha256} | fixtures sha256: ${report.fixtures_sha256}`,
    `attempted ${report.attempted}/${report.expected} complete=${report.complete} scored=${report.scored}`,
    `failures ${report.failures.total} ${JSON.stringify(report.failures.by_kind)}`,
    "per label (n / verdict / target / evidence / core / full):",
  ];
  for (const label of VERDICTS) {
    const b = report.per_label[label];
    lines.push(
      `  ${label.padEnd(26)} ${b.n} / ${b.verdict_exact} / ${b.target_exact} / ${b.evidence_exact} / ${b.core_exact} / ${b.full_exact}`,
    );
  }
  lines.push(
    `false_invalidation=${report.false_invalidation} missed_invalidation=${report.missed_invalidation} wrong_target_given_verdict=${report.wrong_target_given_verdict} target_mismatch_total=${report.target_mismatch_total} wrong_mode_or_time=${report.wrong_mode_or_time} unresolved_missed=${report.unresolved_missed} unresolved_invalidated=${report.unresolved_invalidated} advisory_basis_mismatch=${report.advisory_basis_mismatch}`,
  );
  return lines.join("\n");
}

async function main() {
  const opts = parseArgs(process.argv.slice(2));
  await mkdir(opts.outDir, { recursive: true, mode: DIR_MODE });
  const result = await runPilot(opts);
  console.log(summarize(result.aggregate));
  console.log(`rows: ${result.rowsPath}`);
  console.log(`aggregate: ${result.aggregatePath}`);
  if (!result.aggregate.complete) process.exitCode = 1;
}

if (import.meta.main) {
  await main();
}
